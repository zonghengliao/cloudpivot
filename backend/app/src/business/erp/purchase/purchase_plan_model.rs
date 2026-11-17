//! 采购计划数据层（M4-01 表结构 · M4-02 CRUD + 审批）。
//!
//! 表结构唯一依据：`backend/app/migrations/business/V0446__purchase_plan.up.sql`；
//! 审批流程 / 菜单 / 授权见 `V0447__purchase_plan_crud.up.sql`。
//!
//! | 表 | 行数级 | 关键不变量 |
//! |----|--------|------------|
//! | `biz_purchase_plan` | 每期每部门 1 张，年几十张 | 单号存活唯一；`ANNUAL ⇒ plan_month IS NULL`、`MONTHLY ⇒ 1..12`；「类型 + 年度 + 月 + 编制部门」存活唯一；`SUBMITTED` 必有审批实例 |
//! | `biz_purchase_plan_item` | 一单几十~几千行 | `(plan_id, material_id, warehouse_id)` 唯一（**卡面的粒度**）；`planned_qty > 0`；`executed_qty >= 0`（刻意不拦超计划） |
//!
//! # 本文件到哪（M4-01 表结构 → M4-02 写入口）
//!
//! ```text
//! 有：表名 / 列清单 / 行结构 / 取号（biz_seq PPL）/ 读入口 / 合计重算
//!     + 建单 create_plan / 整单替换 replace_plan / 软删 soft_delete_plan
//!     + 状态流转 mark_submitted / mark_approved / mark_back_to_draft / mark_skipped_approval
//!     + 分页 page_plans / 父单事实 parent_facts
//! 没有：转采购申请（M4-03）· 执行率报表（M4-04）· 前端页面（M4-06）
//! ```
//!
//! 写入口的三条硬口径（M4-01 交接注释逐条落地）：
//! ① 单头三个 `total_*` 是派生列，一律 [`recalc_totals`] 重算，请求体里的同名值不读；
//! ② 状态流转全部 CAS（`WHERE status = ?`），0 行 = 并发/状态已变 → 调用方 409；
//! ③ `executed_qty` 只兜 ≥ 0，**刻意允许超计划**（执行率 >100% 是 M4-04 的报表话题）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_purchase_plan / _item 与采购申请单同口径 —— 计划是公司级滚动文件
//!         （采购要看全公司），`dept_id` 是「编制部门」快照语义而不是数据范围列，
//!         所以不登记 shared::scope::SCOPED_TABLES。可见性由 M4-02 的权限码控制。
//! ```

use nik::prelude::chrono::{DateTime, Duration, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use cloudpivot_kernel::business::shared::scope;

// ==================== 表名 / 取号 / 枚举值 ====================

/// 计划单头表。
pub const PLAN_TABLE: &str = "biz_purchase_plan";
/// 计划明细表。
pub const ITEM_TABLE: &str = "biz_purchase_plan_item";
/// 取号键（`biz_seq.seq_type`）。
pub const SEQ_TYPE: &str = "PPL";
/// 单号前缀。
pub const NO_PREFIX: &str = "PPL";
/// 流水号宽度。
pub const NO_SEQ_WIDTH: usize = 6;

/// 计划类型：年度。
pub const PLAN_TYPE_ANNUAL: &str = "ANNUAL";
/// 计划类型：月度。
pub const PLAN_TYPE_MONTHLY: &str = "MONTHLY";
/// 类型闭集（与库层 `ck_biz_pp_type` 同一份）。
pub const PLAN_TYPES: &[&str] = &[PLAN_TYPE_ANNUAL, PLAN_TYPE_MONTHLY];

/// 状态：编制中。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 状态：审批中（必有审批实例）。
pub const STATUS_SUBMITTED: &str = "SUBMITTED";
/// 状态：已批准（可转采购申请）。
pub const STATUS_APPROVED: &str = "APPROVED";
/// 状态闭集（与库层 `ck_biz_pp_status` 同一份；**只有三个**，不造 CLOSED 这类没有入口的死状态）。
pub const STATUSES: &[&str] = &[STATUS_DRAFT, STATUS_SUBMITTED, STATUS_APPROVED];

// ==================== 行结构 ====================

/// 计划单头行（列表 / 详情用；`item_count` 是派生列）。
#[derive(Debug, Clone)]
pub struct PlanRow {
    /// 主键。
    pub id: Uuid,
    /// 计划单号（`PPL-yyyyMMdd-000001`）。
    pub plan_no: String,
    /// 计划名称。
    pub plan_name: String,
    /// 计划类型：ANNUAL / MONTHLY。
    pub plan_type: String,
    /// 计划年度。
    pub plan_year: i32,
    /// 月度计划的月份（年度计划为 None）。
    pub plan_month: Option<i32>,
    /// 月度计划挂到的年度计划。
    pub parent_plan_id: Option<Uuid>,
    /// 月度计划挂到的年度计划**单号**（实时 join；详情 / 列表直接展示）。
    pub parent_plan_no: Option<String>,
    /// 编制部门。
    pub dept_id: Option<Uuid>,
    /// 编制部门名快照。
    pub dept_name: Option<String>,
    /// 编制人。
    pub planner_id: Option<Uuid>,
    /// 编制人姓名快照。
    pub planner_name: Option<String>,
    /// 状态。
    pub status: String,
    /// 审批实例。
    pub approval_instance_id: Option<Uuid>,
    /// 计划量合计（服务端按明细重算）。
    pub total_planned_qty: Decimal,
    /// 已执行量合计（服务端按明细重算）。
    pub total_executed_qty: Decimal,
    /// 计划金额合计（不含税）。
    pub total_planned_amount: Decimal,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 明细行数（派生）。
    pub item_count: i64,
}

/// 计划明细行（一行 = 物料 × 仓库；`*_code` / `*_name` 由 join 实时取名，按约定 13）。
#[derive(Debug, Clone)]
pub struct PlanItemRow {
    /// 主键。
    pub id: Uuid,
    /// 所属计划。
    pub plan_id: Uuid,
    /// 行号（1 起，单内唯一）。
    pub line_no: i32,
    /// 物料。
    pub material_id: Uuid,
    /// 物料编码（join）。
    pub material_code: String,
    /// 物料名称（join）。
    pub material_name: String,
    /// 仓库。
    pub warehouse_id: Uuid,
    /// 仓库名称（join）。
    pub warehouse_name: String,
    /// 计量单位。
    pub unit_id: Uuid,
    /// 单位名称（join）。
    pub unit_name: String,
    /// 计划量。
    pub planned_qty: Decimal,
    /// 已执行量。
    pub executed_qty: Decimal,
    /// 预估不含税单价。
    pub planned_price: Decimal,
    /// 计划金额（不含税）。
    pub planned_amount: Decimal,
    /// 行备注。
    pub remark: Option<String>,
}

// ==================== 取号 ====================

/// 组装单号（`PPL-{周期键}-{6 位流水}`）。
pub fn format_plan_no(period_key: &str, no: i64) -> String {
    format!("{NO_PREFIX}-{period_key}-{no:0NO_SEQ_WIDTH$}")
}

/// 周期键（北京时间 `yyyyMMdd`）—— 与采购申请 / 采购订单同一口径。
pub fn period_key_of(now: DateTime<Utc>) -> String {
    let cst = now + Duration::hours(8);
    cst.format("%Y%m%d").to_string()
}

/// `biz_seq(seq_type='PPL')` 缺失时幂等补一行（迁移已种，这里是自愈兜底）。
async fn ensure_seq_row(ex: &impl SqlExecutor) -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, 'PPL', '采购计划单号', 'PPL', 'yyyyMMdd', '', 0, 1, '-', 1, \
                '采购计划单号：PPL-yyyyMMdd-000001，按天（CST）滚动' \
          WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'PPL' AND deleted_at IS NULL)"
            .to_string(),
        vec![Value::from(Uuid::now_v7())],
    )
    .await
    .or_else(|error| match error {
        DbError::UniqueViolation { .. } => Ok(0),
        other => Err(other),
    })?;
    Ok(())
}

/// 推进 `biz_seq(seq_type='PPL')` 一个号（同日累加 / 新日重置 / 时钟回拨不重置）。
async fn bump_seq(ex: &impl SqlExecutor, period_key: &str) -> DbResult<Option<i64>> {
    // SCOPE-EXEMPT: 同上。
    let rows = ex
        .query_sql(
            "UPDATE biz_seq \
                SET current_no = CASE WHEN period_key = ? THEN current_no + 1 \
                                      WHEN period_key < ? THEN 1 \
                                      ELSE current_no + 1 END, \
                    period_key = CASE WHEN period_key < ? THEN ? ELSE period_key END, \
                    updated_at = now() \
              WHERE seq_type = 'PPL' AND deleted_at IS NULL \
             RETURNING current_no"
                .to_string(),
            vec![
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
            ],
        )
        .await?;
    rows.first().map(|row| row.get_typed("current_no")).transpose()
}

/// 取一个计划单号（周期键按北京时间）。返回形如 `PPL-20261117-000001`。
pub async fn next_plan_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    let period_key = period_key_of(now);
    ensure_seq_row(ex).await?;
    match bump_seq(ex, &period_key).await? {
        Some(no) => Ok(format_plan_no(&period_key, no)),
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='PPL') 行不存在（period_key={period_key}）"
        ))),
    }
}

// ==================== 读 ====================

/// 单头 select（带明细计数 + 父计划单号）。
fn plan_select_sql() -> String {
    format!(
        "SELECT p.id, p.plan_no, p.plan_name, p.plan_type, p.plan_year, p.plan_month, p.parent_plan_id, \
                par.plan_no AS parent_plan_no, \
                p.dept_id, p.dept_name, p.planner_id, p.planner_name, p.status, p.approval_instance_id, \
                p.total_planned_qty, p.total_executed_qty, p.total_planned_amount, p.remark, \
                p.created_at, p.create_by, \
                (SELECT count(*) FROM {ITEM_TABLE} it WHERE it.plan_id = p.id) AS item_count \
           FROM {PLAN_TABLE} p \
           LEFT JOIN {PLAN_TABLE} par ON par.id = p.parent_plan_id AND par.deleted_at IS NULL \
          WHERE p.deleted_at IS NULL"
    )
}

fn read_plan_row(row: &Row) -> DbResult<PlanRow> {
    Ok(PlanRow {
        id: row.get_typed("id")?,
        plan_no: row.get_typed("plan_no")?,
        plan_name: row.get_typed("plan_name")?,
        plan_type: row.get_typed("plan_type")?,
        plan_year: row.get_typed("plan_year")?,
        plan_month: row.get_typed("plan_month")?,
        parent_plan_id: row.get_typed("parent_plan_id")?,
        parent_plan_no: row.get_typed("parent_plan_no")?,
        dept_id: row.get_typed("dept_id")?,
        dept_name: row.get_typed("dept_name")?,
        planner_id: row.get_typed("planner_id")?,
        planner_name: row.get_typed("planner_name")?,
        status: row.get_typed("status")?,
        approval_instance_id: row.get_typed("approval_instance_id")?,
        total_planned_qty: row.get_typed("total_planned_qty")?,
        total_executed_qty: row.get_typed("total_executed_qty")?,
        total_planned_amount: row.get_typed("total_planned_amount")?,
        remark: row.get_typed("remark")?,
        created_at: row.get_typed("created_at")?,
        create_by: row.get_typed("create_by")?,
        item_count: row.get_typed("item_count")?,
    })
}

/// 按主键取一张存活的计划（不存在 / 已软删 → None）。
pub async fn find_plan(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<PlanRow>> {
    // SCOPE-EXEMPT: 按主键取单行，不涉及数据范围（见模块头）。
    let rows = ex
        .query_sql(
            format!("{} AND p.id = ?::uuid", plan_select_sql()),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(read_plan_row).transpose()
}

/// 按「类型 + 年度 + 月 + 编制部门」取存活计划 —— 这四列正是
/// `uk_biz_purchase_plan_period` 的唯一键，M4-02 新建前用它判重（比撞库报 500 友好）。
pub async fn find_plan_by_period(
    ex: &impl SqlExecutor,
    plan_type: &str,
    plan_year: i32,
    plan_month: Option<i32>,
    dept_id: Option<Uuid>,
) -> DbResult<Option<PlanRow>> {
    // SCOPE-EXEMPT: 唯一键探针，不涉及数据范围。
    let rows = ex
        .query_sql(
            format!(
                "{} AND p.plan_type = ? AND p.plan_year = ? \
                    AND COALESCE(p.plan_month, 0) = COALESCE(?::int, 0) \
                    AND COALESCE(p.dept_id, '00000000-0000-0000-0000-000000000000'::uuid) \
                        = COALESCE(NULLIF(?::text, '')::uuid, '00000000-0000-0000-0000-000000000000'::uuid)",
                plan_select_sql()
            ),
            vec![
                Value::Text(plan_type.to_string()),
                Value::I32(plan_year),
                match plan_month {
                    Some(month) => Value::I32(month),
                    None => Value::I32(0),
                },
                uuid_text(dept_id),
            ],
        )
        .await?;
    rows.first().map(read_plan_row).transpose()
}

/// 一张计划的全部明细（按行号升序；物料 / 仓库 / 单位名实时 join）。
pub async fn items_of_plan(ex: &impl SqlExecutor, plan_id: Uuid) -> DbResult<Vec<PlanItemRow>> {
    // SCOPE-EXEMPT: 明细随单头可见性走（见模块头）。
    let rows = ex
        .query_sql(
            format!(
                "SELECT it.id, it.plan_id, it.line_no, it.material_id, \
                        COALESCE(m.material_code, '') AS material_code, \
                        COALESCE(m.material_name, '') AS material_name, \
                        it.warehouse_id, COALESCE(w.warehouse_name, '') AS warehouse_name, \
                        it.unit_id, COALESCE(u.unit_name, '') AS unit_name, \
                        it.planned_qty, it.executed_qty, it.planned_price, it.planned_amount, it.remark \
                   FROM {ITEM_TABLE} it \
                   LEFT JOIN biz_material m ON m.id = it.material_id AND m.deleted_at IS NULL \
                   LEFT JOIN biz_warehouse w ON w.id = it.warehouse_id AND w.deleted_at IS NULL \
                   LEFT JOIN biz_unit u ON u.id = it.unit_id \
                  WHERE it.plan_id = ?::uuid \
                  ORDER BY it.line_no"
            ),
            vec![Value::from(&plan_id)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(PlanItemRow {
                id: row.get_typed("id")?,
                plan_id: row.get_typed("plan_id")?,
                line_no: row.get_typed("line_no")?,
                material_id: row.get_typed("material_id")?,
                material_code: row.get_typed("material_code")?,
                material_name: row.get_typed("material_name")?,
                warehouse_id: row.get_typed("warehouse_id")?,
                warehouse_name: row.get_typed("warehouse_name")?,
                unit_id: row.get_typed("unit_id")?,
                unit_name: row.get_typed("unit_name")?,
                planned_qty: row.get_typed("planned_qty")?,
                executed_qty: row.get_typed("executed_qty")?,
                planned_price: row.get_typed("planned_price")?,
                planned_amount: row.get_typed("planned_amount")?,
                remark: row.get_typed("remark")?,
            })
        })
        .collect()
}

// ==================== 写（仅表结构的配套：合计重算） ====================

/// 把明细的三个合计回写到单头（`Σ planned_qty` / `Σ executed_qty` / `Σ planned_amount`）。
///
/// 口径：
/// * 单头的三个 `total_*` 是**派生列**，客户端永远不许直接写；建单 / 改明细 / 执行量回写
///   之后都调用它重算（M4-02 / M4-03 / M4-04 各一处）。
/// * 一条 UPDATE ... FROM 子查询完成，不做「先查后写」——避免两个请求各读一次旧值再各写一遍。
/// * 返回 `false` 表示计划不存在或已软删（调用方按 404 处理，而不是静默成功）。
pub async fn recalc_totals(ex: &impl SqlExecutor, plan_id: Uuid) -> DbResult<bool> {
    // SCOPE-EXEMPT: 按主键重算派生的合计列，不涉及数据范围。
    let affected = ex
        .exec_sql(
            format!(
                "UPDATE {PLAN_TABLE} p \
                    SET total_planned_qty = s.qty, total_executed_qty = s.exec_qty, \
                        total_planned_amount = s.amount, updated_at = now() \
                   FROM (SELECT COALESCE(SUM(planned_qty), 0) AS qty, \
                                COALESCE(SUM(executed_qty), 0) AS exec_qty, \
                                COALESCE(SUM(planned_amount), 0) AS amount \
                           FROM {ITEM_TABLE} WHERE plan_id = ?::uuid) s \
                  WHERE p.id = ?::uuid AND p.deleted_at IS NULL"
            ),
            vec![Value::from(&plan_id), Value::from(&plan_id)],
        )
        .await?;
    Ok(affected > 0)
}

/// `Option<Uuid>` → `?::text::uuid` 参数（None = 空串 → NULL，与采购申请单同口径）。
pub fn uuid_text(value: Option<Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

// ==================== 写：建单 / 改单 / 删单 / 状态流转（M4-02） ====================
//
// 单头三个 `total_*` 是派生列：建单 / 改明细后一律由 [`recalc_totals`] 重算，
// 请求体里的同名值**不读**（见 `purchase_plan_dto` 的模块文档 ②）。

/// 分页锚点（SCOPE-EXEMPT，理由见模块文档「数据权限」）。
pub const PLAN_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(PLAN_TABLE);

/// 建单入参（单头）。
#[derive(Debug, Clone)]
pub struct NewPlanHead {
    /// 计划名称。
    pub plan_name: String,
    /// ANNUAL / MONTHLY。
    pub plan_type: String,
    /// 年度。
    pub plan_year: i32,
    /// 月份（年度计划为 None）。
    pub plan_month: Option<i32>,
    /// 父年度计划。
    pub parent_plan_id: Option<Uuid>,
    /// 编制部门。
    pub dept_id: Option<Uuid>,
    /// 编制部门名快照。
    pub dept_name: Option<String>,
    /// 编制人。
    pub planner_id: Option<Uuid>,
    /// 编制人名快照。
    pub planner_name: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// 建单入参（明细行；`unit_id` 由物料带出，`planned_amount` 由服务端算）。
#[derive(Debug, Clone)]
pub struct NewPlanItem {
    /// 物料。
    pub material_id: Uuid,
    /// 仓库。
    pub warehouse_id: Uuid,
    /// 计量单位（物料基础单位）。
    pub unit_id: Uuid,
    /// 计划量（> 0）。
    pub planned_qty: Decimal,
    /// 预估不含税单价（≥ 0）。
    pub planned_price: Decimal,
    /// 行备注。
    pub remark: Option<String>,
}

impl NewPlanItem {
    /// 行金额（4 位舍入）：`planned_qty × planned_price`。
    pub fn planned_amount(&self) -> Decimal {
        (self.planned_qty * self.planned_price).round_dp(4)
    }
}

/// 单头 update 列（改单用；`None` 不是「不改」，而是「改成 NULL」——
/// 改单是**整单替换**，见 `purchase_plan_handler::update_plan`）。
fn head_columns(head: &NewPlanHead) -> Vec<Value> {
    vec![
        Value::Text(head.plan_name.clone()),
        Value::Text(head.plan_type.clone()),
        Value::I32(head.plan_year),
        match head.plan_month {
            Some(month) => Value::I32(month),
            None => Value::Text(String::new()),
        },
        uuid_text(head.parent_plan_id),
        uuid_text(head.dept_id),
        optional_text(head.dept_name.as_deref()),
        uuid_text(head.planner_id),
        optional_text(head.planner_name.as_deref()),
        optional_text(head.remark.as_deref()),
    ]
}

/// 建单（单头 + 明细 + 合计重算，同一事务），返回计划 id。
pub async fn create_plan(
    head: &NewPlanHead,
    items: &[NewPlanItem],
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let tx = db().begin().await?;
    let result = async {
        let plan_no = next_plan_no(&tx, now).await?;
        let id = Uuid::now_v7();
        let mut params = vec![Value::from(&id), Value::Text(plan_no)];
        params.extend(head_columns(head));
        params.push(Value::from(now));
        params.push(Value::from(now));
        params.push(uuid_text(operator));
        params.push(uuid_text(operator));
        tx.exec_sql(
            format!(
                "INSERT INTO {PLAN_TABLE} \
                    (id, plan_no, plan_name, plan_type, plan_year, plan_month, parent_plan_id, \
                     dept_id, dept_name, planner_id, planner_name, remark, \
                     status, created_at, updated_at, create_by, update_by) \
                 VALUES (?, ?, ?, ?, ?, NULLIF(?::text,'')::int, NULLIF(?::text,'')::uuid, \
                     NULLIF(?::text,'')::uuid, NULLIF(?::text,''), \
                     NULLIF(?::text,'')::uuid, NULLIF(?::text,''), NULLIF(?::text,''), \
                     'DRAFT', ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
            ),
            params,
        )
        .await?;
        insert_items(&tx, id, items).await?;
        recalc_totals(&tx, id).await?;
        Ok::<Uuid, DbError>(id)
    }
    .await;
    match result {
        Ok(id) => {
            tx.commit().await?;
            Ok(id)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 插入明细（建单 / 改单共用；行号 1 起，金额由服务端算）。
async fn insert_items(ex: &impl SqlExecutor, plan_id: Uuid, items: &[NewPlanItem]) -> DbResult<()> {
    for (index, item) in items.iter().enumerate() {
        let line_no = i32::try_from(index + 1).unwrap_or(i32::MAX);
        ex.exec_sql(
            format!(
                "INSERT INTO {ITEM_TABLE} \
                    (id, plan_id, line_no, material_id, warehouse_id, unit_id, \
                     planned_qty, executed_qty, planned_price, planned_amount, remark) \
                 VALUES (?, ?::uuid, ?, ?::uuid, ?::uuid, ?::uuid, \
                     ?::text::numeric, 0, ?::text::numeric, ?::text::numeric, NULLIF(?::text,''))"
            ),
            vec![
                Value::from(Uuid::now_v7()),
                Value::from(&plan_id),
                Value::I32(line_no),
                Value::from(&item.material_id),
                Value::from(&item.warehouse_id),
                Value::from(&item.unit_id),
                Value::Text(item.planned_qty.to_string()),
                Value::Text(item.planned_price.to_string()),
                Value::Text(item.planned_amount().to_string()),
                optional_text(item.remark.as_deref()),
            ],
        )
        .await?;
    }
    Ok(())
}

/// 锁单头并读状态（提交 / 改单事务的第一句；不带 JOIN，`FOR UPDATE` 才能用）。
pub async fn lock_plan(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<(String, Option<Uuid>)>> {
    // SCOPE-EXEMPT: 按主键取单行状态并加行锁，不涉及数据范围。
    let rows = ex
        .query_sql(
            format!(
                "SELECT status, approval_instance_id FROM {PLAN_TABLE} \
                  WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some((row.get_typed("status")?, row.get_typed("approval_instance_id")?))),
    }
}

/// 改单（整单替换：单头 + 全量明细 + 合计重算，同一事务由调用方持有）。
///
/// CAS 口径：`WHERE status = 'DRAFT'`；返回 0 行说明单据已不是草稿（并发提交 / 已被批）
/// → 调用方 409。**只有草稿能改**：审批中的单改了会让审批人看的和落库的不是一张。
pub async fn replace_plan(
    ex: &impl SqlExecutor,
    id: Uuid,
    head: &NewPlanHead,
    items: &[NewPlanItem],
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let mut params = head_columns(head);
    params.push(Value::from(now));
    params.push(uuid_text(operator));
    params.push(Value::from(&id));
    let affected = ex
        .exec_sql(
            format!(
                "UPDATE {PLAN_TABLE} \
                    SET plan_name = ?, plan_type = ?, plan_year = ?, \
                        plan_month = NULLIF(?::text,'')::int, \
                        parent_plan_id = NULLIF(?::text,'')::uuid, \
                        dept_id = NULLIF(?::text,'')::uuid, dept_name = NULLIF(?::text,''), \
                        planner_id = NULLIF(?::text,'')::uuid, planner_name = NULLIF(?::text,''), \
                        remark = NULLIF(?::text,''), \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = 'DRAFT' AND deleted_at IS NULL"
            ),
            params,
        )
        .await?;
    if affected == 0 {
        return Ok(0);
    }
    ex.exec_sql(
        format!("DELETE FROM {ITEM_TABLE} WHERE plan_id = ?::uuid"),
        vec![Value::from(&id)],
    )
    .await?;
    insert_items(ex, id, items).await?;
    recalc_totals(ex, id).await?;
    Ok(affected)
}

/// 删单（软删；只有草稿能删）。返回 0 行 = 不是草稿 / 不存在。
///
/// 为什么不物理删：明细带 `executed_qty`（执行量可能已被 M4-03 引用），
/// 物理删会让「这张计划到底有没有过」查不出来；软删保住审计线索。
pub async fn soft_delete_plan(
    ex: &impl SqlExecutor,
    id: Uuid,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {PLAN_TABLE} \
                SET deleted_at = ?, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status = 'DRAFT' AND deleted_at IS NULL"
        ),
        vec![
            Value::from(now),
            Value::from(now),
            uuid_text(operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 提交审批：`DRAFT → SUBMITTED` + 审批实例 id（同一事务由调用方持有）。
pub async fn mark_submitted(
    ex: &impl SqlExecutor,
    id: Uuid,
    instance_id: Uuid,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    apply_status_with_instance(
        ex,
        id,
        &Transition {
            from: STATUS_DRAFT,
            to: STATUS_SUBMITTED,
            instance: Some(instance_id),
            reason,
        },
        operator,
        now,
    )
    .await
}

/// 审批结论回写：`SUBMITTED → APPROVED`（通过）。
pub async fn mark_approved(
    ex: &impl SqlExecutor,
    id: Uuid,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    apply_status_with_instance(
        ex,
        id,
        &Transition {
            from: STATUS_SUBMITTED,
            to: STATUS_APPROVED,
            instance: None,
            reason,
        },
        operator,
        now,
    )
    .await
}

/// 审批结论回写：`SUBMITTED → DRAFT`（驳回 / 撤回打回草稿，清实例）。
pub async fn mark_back_to_draft(
    ex: &impl SqlExecutor,
    id: Uuid,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    apply_status_with_instance(
        ex,
        id,
        &Transition {
            from: STATUS_SUBMITTED,
            to: STATUS_DRAFT,
            instance: None,
            reason,
        },
        operator,
        now,
    )
    .await
}

/// 显式跳过审批：`DRAFT → APPROVED`（**没有**审批实例）。
///
/// 只有「没配已发布的 `pp_approve` 流程」且操作人持 `business:erp:skip-approval`
/// 且理由 ≥10 字时才走这条路（判据在 `approval_gate::authorize_missing_flow`）；
/// 理由随状态流转写进备注，审计据此追溯「为什么这次没有复核」。
pub async fn mark_skipped_approval(
    ex: &impl SqlExecutor,
    id: Uuid,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    apply_status_with_instance(
        ex,
        id,
        &Transition {
            from: STATUS_DRAFT,
            to: STATUS_APPROVED,
            instance: None,
            reason,
        },
        operator,
        now,
    )
    .await
}

/// 一次状态流转的参数（把 8 个参数收成 6 个：clippy 的上限是 7）。
struct Transition<'a> {
    /// 期望的当前状态（CAS 条件）。
    from: &'a str,
    /// 目标状态。
    to: &'a str,
    /// 审批实例：`Some` 写入、`None` 清空。
    instance: Option<Uuid>,
    /// 追加到备注的说明。
    reason: Option<&'a str>,
}

/// 状态流转（CAS：`from` → `to`；`instance` 有值则写、无值则清空）。
async fn apply_status_with_instance(
    ex: &impl SqlExecutor,
    id: Uuid,
    transition: &Transition<'_>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let Transition {
        from,
        to,
        instance,
        reason,
    } = *transition;
    let set_instance = match instance {
        Some(_) => "approval_instance_id = NULLIF(?::text,'')::uuid,",
        None => "approval_instance_id = NULL,",
    };
    let mut params = vec![Value::Text(to.to_string())];
    if instance.is_some() {
        params.push(uuid_text(instance));
    }
    params.extend([
        optional_text(reason),
        optional_text(reason),
        optional_text(reason),
        Value::from(now),
        uuid_text(operator),
        Value::from(&id),
        Value::Text(from.to_string()),
    ]);
    ex.exec_sql(
        format!(
            "UPDATE {PLAN_TABLE} \
                SET status = ?, {set_instance} \
                    remark = CASE WHEN ?::text IS NULL OR btrim(?::text) = '' THEN remark \
                                  ELSE COALESCE(NULLIF(remark, '') || E'\\n', '') || ?::text END, \
                    updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
        ),
        params,
    )
    .await
}

// ==================== 读：分页 / 父单事实 / 主数据 ====================

/// 分页查询（关键字 / 状态 / 类型 / 年度 / 部门）。
///
/// 不吃执行器：`scope::paginate_unscoped` 走全局 `db()`（与采购申请分页同款）。
pub async fn page_plans(
    filter: &crate::business::erp::purchase::purchase_plan_dto::PlanFilter,
) -> DbResult<(Vec<PlanRow>, u64)> {
    let mut sql = plan_select_sql();
    let mut params: Vec<Value> = Vec::new();
    if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND p.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(plan_type) = filter.plan_type.as_deref() {
        sql.push_str(" AND p.plan_type = ?");
        params.push(Value::Text(plan_type.to_string()));
    }
    if let Some(year) = filter.plan_year {
        sql.push_str(" AND p.plan_year = ?");
        params.push(Value::I32(year));
    }
    if let Some(dept_id) = filter.dept_id {
        sql.push_str(" AND p.dept_id = ?::uuid");
        params.push(Value::from(&dept_id));
    }
    if let Some(pattern) = cloudpivot_kernel::utils::like_pattern(filter.keyword.as_deref()) {
        let like = Value::Text(pattern);
        sql.push_str(
            " AND (p.plan_no LIKE ? OR p.plan_name LIKE ? OR COALESCE(p.remark, '') LIKE ? \
                OR COALESCE(p.dept_name, '') LIKE ? \
                OR EXISTS (SELECT 1 FROM sys_dept d \
                            WHERE d.id = p.dept_id AND d.dept_name LIKE ?))",
        );
        for _ in 0..5 {
            params.push(like.clone());
        }
    }
    let page = scope::paginate_unscoped(
        PLAN_TARGET,
        &sql,
        params,
        Some("ORDER BY p.plan_year DESC, p.plan_month DESC NULLS LAST, p.plan_no DESC"),
        filter.page,
        filter.page_size,
    )
    .await?;
    let rows = page.list.iter().map(read_plan_row).collect::<DbResult<Vec<_>>>()?;
    Ok((rows, page.total))
}

/// 父单事实：`(plan_type, plan_year, plan_month)`（判 `parent_plan_id` 语义用）。
pub async fn parent_facts(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<(String, i32, Option<i32>)>> {
    // SCOPE-EXEMPT: 按主键取 1 行的类型/年度/月份，不做数据范围裁剪。
    let rows = ex
        .query_sql(
            format!(
                "SELECT plan_type, plan_year, plan_month FROM {PLAN_TABLE} \
                  WHERE id = ?::uuid AND deleted_at IS NULL"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some((
            row.get_typed("plan_type")?,
            row.get_typed("plan_year")?,
            row.get_typed("plan_month")?,
        ))),
    }
}

/// 部门名（建单快照用；查不到 → `None`，调用方按 400 处理）。
pub async fn dept_name_of(ex: &impl SqlExecutor, dept_id: Uuid) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: 按主键取 1 行名称。
    let rows = ex
        .query_sql(
            "SELECT dept_name FROM sys_dept WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(&dept_id)],
        )
        .await?;
    rows.first().map(|row| row.get_typed("dept_name")).transpose()
}

/// 可选文本（`None` / 空串 → `NULLIF(?::text,'')` 折成 NULL）。
pub fn optional_text(value: Option<&str>) -> Value {
    Value::Text(value.unwrap_or_default().to_string())
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 传入带 `+08:00` 偏移的字面量（北京时间），返回 UTC 时刻。
    fn cst(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("rfc3339 时刻字面量")
            .with_timezone(&Utc)
    }

    #[test]
    fn plan_no_is_zero_padded_to_six_digits() {
        assert_eq!(format_plan_no("20261117", 1), "PPL-20261117-000001");
        assert_eq!(format_plan_no("20261117", 42), "PPL-20261117-000042");
        assert_eq!(format_plan_no("20261117", 999999), "PPL-20261117-999999");
    }

    /// 周期键按**北京时间**切天：UTC 的 15:59 还是当天，16:00 就是次日。
    /// 取号一旦按 UTC 切，早上 8 点前建的计划会拿到前一天的号。
    #[test]
    fn period_key_rolls_over_at_cst_midnight() {
        assert_eq!(period_key_of(cst("2026-11-17T23:59:59+08:00")), "20261117");
        assert_eq!(period_key_of(cst("2026-11-18T00:00:00+08:00")), "20261118");
        assert_eq!(period_key_of(cst("2026-11-18T07:59:00+08:00")), "20261118");
        // 同一时刻的 UTC 表示（前一天 15:59Z）必须落到北京时间的当天，而不是 UTC 的前一天。
        assert_eq!(period_key_of(cst("2026-11-17T15:59:59Z")), "20261117");
    }

    #[test]
    fn status_and_type_sets_match_migration_check() {
        assert_eq!(PLAN_TYPES, &["ANNUAL", "MONTHLY"]);
        assert_eq!(STATUSES, &["DRAFT", "SUBMITTED", "APPROVED"]);
        // 「不造死状态」：CLOSED / CANCELLED 刻意不在闭集里（库层 CHECK 会拒）。
        assert!(!STATUSES.contains(&"CLOSED"));
        assert!(!STATUSES.contains(&"CANCELLED"));
    }

    #[test]
    fn uuid_text_maps_none_to_empty_string() {
        match uuid_text(None) {
            Value::Text(text) => assert!(text.is_empty(), "None 必须折成空串（由 ?::text::uuid 转 NULL）"),
            other => panic!("uuid_text(None) 不是 Text：{other:?}"),
        }
        let id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("uuid");
        match uuid_text(Some(id)) {
            Value::Text(text) => assert_eq!(text, id.to_string()),
            other => panic!("uuid_text(Some) 不是 Text：{other:?}"),
        }
    }
}

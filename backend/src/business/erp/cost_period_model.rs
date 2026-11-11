//! 成本核算批次（`biz_cost_period`）数据层 —— 结算四件套骨架
//! （ARCH-9 §3.1 ⑨ / §4.1 / §5.2 #25–#33 · B3-1）。
//!
//! 表结构唯一依据：`backend/migrations/business/V0373__cost_period_tables.up.sql`
//! （已应用，与设计 §3.1 ⑨ 逐字一致）。**本卡不新增迁移**：表、唯一键、状态 CHECK
//! 全在 V0373 里；权限码与菜单由 V0378 种好。所以卡面「迁移幂等 + `.down.sql`」N/A，
//! 也不碰任何已应用迁移（`git status backend/migrations` 必须 0 行）。
//!
//! # 数据权限（设计 D-18 / §5.4）
//!
//! `biz_cost_period` 是**期间合计**单头，登记为 `unscoped` + 权限码
//! （与 `salary_sheet` 同口径：SELF 对期间合计没有意义 —— 一个批次里混着多个车间的
//! 对象，按车间裁剪会把合计改成一个没人能解释的数）。
//! **明细** `biz_cost_period_item` 才是车间维度的（`workshop_dept_id` 在
//! `biz_cost_object` 上），它的裁剪由成本对象侧负责。
//!
//! # 名字怎么补（AGENTS 约定 13）
//!
//! | 列 | 口径 | 本模块做法 |
//! |---|---|---|
//! | `create_by` / `settled_by` / `reversed_by` → 姓名 | 业务展示 ⇒ **join 实时取名** | `LEFT JOIN sys_user`，取 `real_name`，落空回 `username` |
//! | `biz_cost_period_item.cost_object_id` → `object_no` | 主数据关联 ⇒ **join 实时取名** | `LEFT JOIN biz_cost_object` |
//! | `product_id` → 品名 | 主数据关联 ⇒ 实时 | `LEFT JOIN biz_material` |
//!
//! 全部一次 join 拿全，**没有**逐行补名的 N+1。
//!
//! # 批次号为什么自己拼
//!
//! 见 `cost_period_dto::format_batch_no`：形状是 `CS-yyyyMM-0001`（按期间月 + 4 位），
//! 而仓库通用取号 `voucher::next_doc_no` 是「按天 + 6 位」。号段仍然落在同一张
//! `biz_seq` 表（`seq_type = 'COST_PERIOD'`），不另建号段表 —— 取号的两条 SQL
//! 与 `voucher` 同款（`ON CONFLICT DO NOTHING` 补行 + `CASE` 三支推进）。
//!
//! **并发**：`UPDATE biz_seq ... RETURNING current_no` 是行锁，两个并发建单会串行拿到
//! 两个不同的号；即便真的撞了，`uk_biz_cost_period_no`（V0373）也会拒掉，不会出现
//! 两个批次共用一个号。

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_period_dto::{self, CostPeriodFilter, CostPeriodSummary, CostSheetItemRes};
use crate::business::shared::scope;
use crate::business::shared::state_machine;

/// 表名。
pub const TABLE: &str = "biz_cost_period";

/// 本模块读的单头列（与 V0373 ⑨ 逐列对应）。
pub const COLUMNS: &str = "t.id, t.org_id, t.period_code, t.batch_no, t.status, t.labor_source, \
     t.object_count, t.total_material, t.total_labor, t.total_overhead, t.total_cost, \
     t.total_finished, t.total_wip, t.approval_instance_id, t.labor_voucher_id, \
     t.overhead_voucher_id, t.finish_voucher_id, t.settled_at, t.settled_by, t.reversed_at, \
     t.reversed_by, t.reverse_reason, t.cancel_reason, t.remark, t.created_at, t.updated_at, \
     t.create_by, t.update_by";

/// 数据权限锚点：**unscoped**（见模块头「数据权限」）。
pub const TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(TABLE);

/// 列表全序：新的期间在前；同期间用批次号 / id 收全序（没有全序翻页会漏行 / 重复行）。
pub const ORDER: &str = "ORDER BY t.period_code DESC, t.batch_no DESC, t.id ASC";

/// 合法边表：**必须**与 `state_machine` 注册表逐条一致
/// （`transition` 会核对，漂了就是 500 —— 这是故意的：边表只有一份真相）。
pub const LEGAL_EDGES: [(&str, &str); 7] = state_machine::COST_PERIOD_EDGES;

/// 号段类型（`biz_seq.seq_type`）。与凭证取号共用 `biz_seq`，不另建表。
pub const SEQ_TYPE: &str = "COST_PERIOD";

/// 号段名（首次建行时写入 `biz_seq.seq_name`）。
pub const SEQ_NAME: &str = "成本核算批次号";

/// 号段说明。
pub const SEQ_REMARK: &str = "成本核算批次号：CS-yyyyMM-0001，按期间月（不止按天）滚动";

/// 单头行（+ join 出来的姓名）。
#[derive(Debug, Clone)]
pub struct CostPeriodRow {
    /// 主键。
    pub id: Uuid,
    /// 机构（多组织哨兵）。
    pub org_id: Uuid,
    /// 期间 `yyyy-MM`。
    pub period_code: String,
    /// 批次号。
    pub batch_no: String,
    /// 状态。
    pub status: String,
    /// 人工来源。
    pub labor_source: String,
    /// 成本对象数（落库值）。
    pub object_count: i32,
    /// 材料合计（落库值）。
    pub total_material: Decimal,
    /// 人工合计（落库值）。
    pub total_labor: Decimal,
    /// 制造费用合计（落库值）。
    pub total_overhead: Decimal,
    /// 投入合计（落库值）。
    pub total_cost: Decimal,
    /// 完工转出合计（落库值）。
    pub total_finished: Decimal,
    /// 期末在制合计（落库值）。
    pub total_wip: Decimal,
    /// 审批实例。
    pub approval_instance_id: Option<Uuid>,
    /// 人工计提凭证。
    pub labor_voucher_id: Option<Uuid>,
    /// 制造费用凭证。
    pub overhead_voucher_id: Option<Uuid>,
    /// 完工结转凭证。
    pub finish_voucher_id: Option<Uuid>,
    /// 结算时间。
    pub settled_at: Option<DateTime<Utc>>,
    /// 结算人。
    pub settled_by: Option<Uuid>,
    /// 红冲时间。
    pub reversed_at: Option<DateTime<Utc>>,
    /// 红冲人。
    pub reversed_by: Option<Uuid>,
    /// 红冲原因。
    pub reverse_reason: Option<String>,
    /// 作废原因。
    pub cancel_reason: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 制单人。
    pub create_by: Option<Uuid>,
    /// 最后修改人。
    pub update_by: Option<Uuid>,
    // ---- join ----
    /// 制单人姓名。
    pub create_by_name: Option<String>,
    /// 结算人姓名。
    pub settled_by_name: Option<String>,
    /// 红冲人姓名。
    pub reversed_by_name: Option<String>,
}

impl CostPeriodRow {
    /// 行 → 结构（列缺失就是 DDL 与代码漂了，交给上层 500）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            org_id: row.get_typed("org_id")?,
            period_code: row.get_typed("period_code")?,
            batch_no: row.get_typed("batch_no")?,
            status: row.get_typed("status")?,
            labor_source: row.get_typed("labor_source")?,
            object_count: row.get_typed("object_count")?,
            total_material: row.get_typed("total_material")?,
            total_labor: row.get_typed("total_labor")?,
            total_overhead: row.get_typed("total_overhead")?,
            total_cost: row.get_typed("total_cost")?,
            total_finished: row.get_typed("total_finished")?,
            total_wip: row.get_typed("total_wip")?,
            approval_instance_id: row.get_typed("approval_instance_id")?,
            labor_voucher_id: row.get_typed("labor_voucher_id")?,
            overhead_voucher_id: row.get_typed("overhead_voucher_id")?,
            finish_voucher_id: row.get_typed("finish_voucher_id")?,
            settled_at: row.get_typed("settled_at")?,
            settled_by: row.get_typed("settled_by")?,
            reversed_at: row.get_typed("reversed_at")?,
            reversed_by: row.get_typed("reversed_by")?,
            reverse_reason: row.get_typed("reverse_reason")?,
            cancel_reason: row.get_typed("cancel_reason")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
            update_by: row.get_typed("update_by")?,
            create_by_name: row.get_typed("create_by_name")?,
            settled_by_name: row.get_typed("settled_by_name")?,
            reversed_by_name: row.get_typed("reversed_by_name")?,
        })
    }

    /// 只有 `DRAFT` 可写（设计 §4.1）。
    pub fn writable(&self) -> bool {
        cost_period_dto::WRITABLE_STATUSES.contains(&self.status.as_str())
    }
}

/// 活跃批次摘要（同期间唯一键的前置查询 / calc 编排取批次用）。
#[derive(Debug, Clone, PartialEq)]
pub struct BatchBrief {
    /// 批次主键。
    pub id: Uuid,
    /// 批次号。
    pub batch_no: String,
    /// 状态。
    pub status: String,
}

/// 明细现算合计（`DRAFT` 阶段单头 `total_*` 还是 0，详情要能看见数）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PeriodTotals {
    /// 有计算单行的对象数。
    pub object_count: i64,
    /// 材料合计。
    pub total_material: Decimal,
    /// 人工合计。
    pub total_labor: Decimal,
    /// 制造费用合计。
    pub total_overhead: Decimal,
    /// 投入合计。
    pub total_cost: Decimal,
    /// 完工转出合计。
    pub total_finished: Decimal,
    /// 期末在制合计。
    pub total_wip: Decimal,
}

impl PeriodTotals {
    /// 转出参（金额一律字符串）。
    pub fn to_summary(&self) -> CostPeriodSummary {
        CostPeriodSummary {
            object_count: self.object_count,
            total_material: self.total_material.to_string(),
            total_labor: self.total_labor.to_string(),
            total_overhead: self.total_overhead.to_string(),
            total_cost: self.total_cost.to_string(),
            total_finished: self.total_finished.to_string(),
            total_wip: self.total_wip.to_string(),
        }
    }
}

// ==================== 查询 ====================

/// 单头 SQL 的 SELECT 段（含三个姓名的 join）。
fn select_sql() -> String {
    format!(
        "SELECT {COLUMNS}, \
                COALESCE(NULLIF(uc.real_name, ''), uc.username) AS create_by_name, \
                COALESCE(NULLIF(us.real_name, ''), us.username) AS settled_by_name, \
                COALESCE(NULLIF(ur.real_name, ''), ur.username) AS reversed_by_name \
           FROM {TABLE} t \
           LEFT JOIN sys_user uc ON uc.id = t.create_by \
           LEFT JOIN sys_user us ON us.id = t.settled_by \
           LEFT JOIN sys_user ur ON ur.id = t.reversed_by"
    )
}

/// 按主键取存活行（软删 / 不存在 → `None`）。
pub async fn find_by_id(id: Uuid) -> DbResult<Option<CostPeriodRow>> {
    let sql = format!("{} WHERE t.id = ?::uuid AND t.deleted_at IS NULL", select_sql());
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(CostPeriodRow::from_row).transpose()
}

/// 按期间取**活跃**批次（`uk_biz_cost_period` 的口径：排除软删 / `REVERSED` / `CANCELLED`）。
///
/// 这是「建新批次前先看有没有占位」的唯一入口，也是 calc 编排（#27）取批次的入口。
pub async fn active_by_period(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Option<BatchBrief>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT id, batch_no, status FROM {TABLE} \
                  WHERE period_code = ? AND deleted_at IS NULL \
                    AND status NOT IN ('REVERSED', 'CANCELLED') \
                  ORDER BY created_at DESC LIMIT 1"
            ),
            vec![Value::Text(period_code.to_string())],
        )
        .await?;
    rows.first()
        .map(|row| {
            Ok(BatchBrief {
                id: row.get_typed("id")?,
                batch_no: row.get_typed("batch_no")?,
                status: row.get_typed("status")?,
            })
        })
        .transpose()
}

/// 列表分页（**显式 unscoped**，见模块头）。
pub async fn page(filter: &CostPeriodFilter) -> DbResult<(Vec<CostPeriodRow>, i64)> {
    let mut sql = format!("{} WHERE t.deleted_at IS NULL", select_sql());
    let mut params: Vec<Value> = Vec::new();
    if let Some(period_code) = filter.period_code.as_deref() {
        sql.push_str(" AND t.period_code = ?");
        params.push(Value::Text(period_code.to_string()));
    }
    if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND t.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(pattern) = like_pattern(&filter.keyword) {
        sql.push_str(" AND (t.batch_no LIKE ? OR t.period_code LIKE ? OR COALESCE(t.remark, '') LIKE ?)");
        for _ in 0..3 {
            params.push(Value::from(pattern.clone()));
        }
    }
    // SCOPE-EXEMPT: 期间合计单头是财务全公司口径（设计 D-18 登记为 unscoped）；
    // 分页仍走唯一入口 paginate_unscoped，不自拼 LIMIT/OFFSET。
    let result = scope::paginate_unscoped(TARGET, &sql, params, Some(ORDER), filter.page, filter.page_size).await?;
    let rows = result
        .list
        .iter()
        .map(CostPeriodRow::from_row)
        .collect::<DbResult<Vec<CostPeriodRow>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 关键字 → LIKE 模式（与其余模块同口径，做成小函数便于单测）。
fn like_pattern(keyword: &Option<String>) -> Option<String> {
    keyword
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| format!("%{}%", text.replace('%', "\\%").replace('_', "\\_")))
}

/// 一个批次的成本计算单（对象 × 要素 × 约当 × 单位成本）。
///
/// 名字口径：对象号 / 品名**实时 join**（主数据改名后单据显示新名才是对的）。
pub async fn sheet_items(period_id: Uuid) -> DbResult<Vec<CostSheetItemRes>> {
    let rows = db()
        .find_with_params(
            "SELECT i.id, i.cost_object_id, i.wip_beg_qty, i.wip_beg_cost, i.completed_qty, \
                    i.wip_end_qty, i.completion_pct, i.equiv_material, i.equiv_conversion, \
                    i.mat_cur, i.labor_cur, i.oh_cur, i.unit_material, i.unit_labor, \
                    i.unit_overhead, i.finished_cost, i.wip_end_cost, i.unit_finished_cost, \
                    i.stock_in_id, i.status, \
                    o.object_no, m.material_name \
               FROM biz_cost_period_item i \
               LEFT JOIN biz_cost_object o ON o.id = i.cost_object_id \
               LEFT JOIN biz_material m ON m.id = i.product_id \
              WHERE i.period_id = ?::uuid \
              ORDER BY o.object_no ASC NULLS LAST, i.id ASC",
            vec![Value::from(&period_id)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(CostSheetItemRes {
                id: row.get_typed::<Uuid>("id")?.to_string(),
                cost_object_id: row.get_typed::<Uuid>("cost_object_id")?.to_string(),
                object_no: row.get_typed("object_no")?,
                product_name: row.get_typed("material_name")?,
                wip_beg_qty: row.get_typed::<Decimal>("wip_beg_qty")?.to_string(),
                wip_beg_cost: row.get_typed::<Decimal>("wip_beg_cost")?.to_string(),
                completed_qty: row.get_typed::<Decimal>("completed_qty")?.to_string(),
                wip_end_qty: row.get_typed::<Decimal>("wip_end_qty")?.to_string(),
                completion_pct: row.get_typed::<Decimal>("completion_pct")?.to_string(),
                equiv_material: row.get_typed::<Decimal>("equiv_material")?.to_string(),
                equiv_conversion: row.get_typed::<Decimal>("equiv_conversion")?.to_string(),
                mat_cur: row.get_typed::<Decimal>("mat_cur")?.to_string(),
                labor_cur: row.get_typed::<Decimal>("labor_cur")?.to_string(),
                oh_cur: row.get_typed::<Decimal>("oh_cur")?.to_string(),
                unit_material: row.get_typed::<Decimal>("unit_material")?.to_string(),
                unit_labor: row.get_typed::<Decimal>("unit_labor")?.to_string(),
                unit_overhead: row.get_typed::<Decimal>("unit_overhead")?.to_string(),
                finished_cost: row.get_typed::<Decimal>("finished_cost")?.to_string(),
                wip_end_cost: row.get_typed::<Decimal>("wip_end_cost")?.to_string(),
                unit_finished_cost: row.get_typed::<Decimal>("unit_finished_cost")?.to_string(),
                stock_in_id: row.get_typed::<Option<Uuid>>("stock_in_id")?.map(|id| id.to_string()),
                status: row.get_typed("status")?,
            })
        })
        .collect()
}

/// 明细现算合计（详情用；空明细给全 0）。
pub async fn totals_of(period_id: Uuid) -> DbResult<PeriodTotals> {
    let rows = db()
        .find_with_params(
            "SELECT count(*)::bigint            AS object_count, \
                    COALESCE(sum(mat_cur), 0)        AS total_material, \
                    COALESCE(sum(labor_cur), 0)      AS total_labor, \
                    COALESCE(sum(oh_cur), 0)         AS total_overhead, \
                    COALESCE(sum(mat_cur + labor_cur + oh_cur), 0) AS total_cost, \
                    COALESCE(sum(finished_cost), 0)  AS total_finished, \
                    COALESCE(sum(wip_end_cost), 0)   AS total_wip \
               FROM biz_cost_period_item WHERE period_id = ?::uuid",
            vec![Value::from(&period_id)],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(PeriodTotals {
            object_count: row.get_typed("object_count")?,
            total_material: row.get_typed("total_material")?,
            total_labor: row.get_typed("total_labor")?,
            total_overhead: row.get_typed("total_overhead")?,
            total_cost: row.get_typed("total_cost")?,
            total_finished: row.get_typed("total_finished")?,
            total_wip: row.get_typed("total_wip")?,
        }),
        None => Ok(PeriodTotals::default()),
    }
}

/// 有效边（`BASE_EDGES ∩ 配置`）：详情页的 `next_statuses` 从它来，
/// 管理端禁用某条边之后前端按钮随之消失。
pub async fn effective_edges() -> Vec<(String, String)> {
    state_machine::effective_edges(db(), state_machine::Entity::CostPeriod)
        .await
        .into_iter()
        .map(|edge| (edge.from.to_string(), edge.to.to_string()))
        .collect()
}

/// 该批次的计算单行数（作废前置：有行就不能当作「从没算过」）。
pub async fn item_count(ex: &impl SqlExecutor, period_id: Uuid) -> DbResult<i64> {
    let rows = ex
        .query_sql(
            "SELECT count(*)::bigint AS n FROM biz_cost_period_item WHERE period_id = ?::uuid".to_string(),
            vec![Value::from(&period_id)],
        )
        .await?;
    rows.first()
        .map(|row| row.get_typed("n"))
        .transpose()
        .map(|value| value.unwrap_or(0))
}

// ==================== 取号 ====================

/// 取一个批次号（`CS-yyyyMM-0001`）。
///
/// 两条 SQL 与 `voucher::next_doc_no` 同款（**不能**改成「先查再插」：并发下双单）：
///
/// 1. `INSERT … ON CONFLICT (seq_type) WHERE deleted_at IS NULL DO NOTHING` 补号段行
///    （靠 `uk_biz_seq_type` 这个部分唯一索引；绝不能靠捕获唯一键冲突 —— 会把事务打成 aborted）；
/// 2. `UPDATE … CASE` 三支推进：同月累加 / 新月重置 / 时钟回拨**绝不**重置
///    （回拨时继续 +1，宁可跳号也不撞号）。
pub async fn next_batch_no(ex: &impl SqlExecutor, period_code: &str) -> DbResult<String> {
    let key = cost_period_dto::batch_period_key(period_code)
        .map_err(|error| DbError::InvalidValue(format!("期间不合法（{}）：{error:?}", error.msg)))?;
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                             current_no, step, separator, status, remark) \
         SELECT ?, ?, ?, 'CS', 'yyyyMM', '', 0, 1, '-', 1, ? \
         ON CONFLICT (seq_type) WHERE deleted_at IS NULL DO NOTHING"
            .to_string(),
        vec![
            Value::from(&Uuid::now_v7()),
            Value::Text(SEQ_TYPE.to_string()),
            Value::Text(SEQ_NAME.to_string()),
            Value::Text(SEQ_REMARK.to_string()),
        ],
    )
    .await?;
    // SCOPE-EXEMPT: 同上。
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
                Value::Text(key.clone()),
                Value::Text(key.clone()),
                Value::Text(key.clone()),
                Value::Text(key.clone()),
                Value::Text(SEQ_TYPE.to_string()),
            ],
        )
        .await?;
    match rows.first().map(|row| row.get_typed::<i64>("current_no")).transpose()? {
        Some(no) => Ok(cost_period_dto::format_batch_no(&key, no)),
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='{SEQ_TYPE}') 行不存在（period_key={key}）"
        ))),
    }
}

// ==================== 写入 ====================

/// 建 `DRAFT` 批次（单头合计 / 对象数由结算事务回写，这里落 0）。
pub async fn insert_draft(
    ex: &impl SqlExecutor,
    id: Uuid,
    period_code: &str,
    batch_no: &str,
    labor_source: &str,
    remark: Option<&str>,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    let operator = Value::Text(operator.map(|id| id.to_string()).unwrap_or_default());
    ex.exec_sql(
        format!(
            "INSERT INTO {TABLE} (id, period_code, batch_no, status, labor_source, remark, \
                                  create_by, update_by) \
             VALUES (?, ?, ?, ?, ?, NULLIF(?::text, ''), NULLIF(?::text, '')::uuid, \
                     NULLIF(?::text, '')::uuid)"
        ),
        vec![
            Value::from(&id),
            Value::Text(period_code.to_string()),
            Value::Text(batch_no.to_string()),
            Value::Text(cost_period_dto::STATUS_DRAFT.to_string()),
            Value::Text(labor_source.to_string()),
            Value::Text(remark.map(str::to_string).unwrap_or_default()),
            operator.clone(),
            operator,
        ],
    )
    .await
}

/// 改 `DRAFT` 批次（人工来源 / 备注）。
///
/// **CAS `WHERE status = 'DRAFT'`**：并发下（刚被提交）影响行数为 0，handler 给 409
/// 而不是「改了已冻结的批次」。
pub async fn update_draft(
    ex: &impl SqlExecutor,
    id: Uuid,
    labor_source: &str,
    remark: Option<&str>,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET labor_source = ?, remark = NULLIF(?::text, ''), \
                    updated_at = now(), update_by = NULLIF(?::text, '')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL AND status = 'DRAFT'"
        ),
        vec![
            Value::Text(labor_source.to_string()),
            Value::Text(remark.map(str::to_string).unwrap_or_default()),
            Value::Text(operator.map(|id| id.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

/// 作废（`DRAFT → CANCELLED`）：CAS 保护 + 落原因。
pub async fn apply_cancel(ex: &impl SqlExecutor, id: Uuid, reason: &str, operator: Option<Uuid>) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET status = 'CANCELLED', cancel_reason = ?, updated_at = now(), \
                    update_by = NULLIF(?::text, '')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL AND status = 'DRAFT'"
        ),
        vec![
            Value::Text(reason.to_string()),
            Value::Text(operator.map(|id| id.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

// ==================== 单测（纯逻辑部分） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        use std::str::FromStr;
        Decimal::from_str(text).expect("数值字面量")
    }

    #[test]
    fn table_and_columns_match_v0373() {
        assert_eq!(TABLE, "biz_cost_period", "V0373 ⑨ 的单数表名");
        // 唯一键 / 唯一批次号都在 V0373；这里防「代码里又拼了一个复数表名」
        assert!(!TABLE.ends_with("periods"));
        for column in [
            "period_code",
            "batch_no",
            "labor_source",
            "total_wip",
            "finish_voucher_id",
            "reverse_reason",
            "cancel_reason",
        ] {
            assert!(COLUMNS.contains(column), "单头列漏了 {column}");
        }
    }

    #[test]
    fn legal_edges_are_the_registry() {
        assert_eq!(LEGAL_EDGES, state_machine::COST_PERIOD_EDGES);
        assert_eq!(LEGAL_EDGES.len(), 7);
    }

    #[test]
    fn target_is_unscoped() {
        // 设计 D-18：期间合计单头 unscoped（SELF 对合计没有意义）
        assert!(!TARGET.is_scoped(), "biz_cost_period 不许登记数据权限");
    }

    #[test]
    fn select_sql_joins_three_user_names_once() {
        let sql = select_sql();
        assert_eq!(
            sql.matches("LEFT JOIN sys_user").count(),
            3,
            "制单 / 结算 / 红冲三个姓名"
        );
        assert!(sql.contains("create_by_name"));
        assert!(sql.contains("settled_by_name"));
        assert!(sql.contains("reversed_by_name"));
        // 一次 join 拿全 → 列表不会 N+1
        assert!(!sql.contains("(SELECT"), "别在 SELECT 里塞子查询（N+1 的另一种写法）");
    }

    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(like_pattern(&None), None);
        assert_eq!(like_pattern(&Some("  ".to_string())), None);
        assert_eq!(
            like_pattern(&Some("CS-2026%".to_string())),
            Some("%CS-2026\\%%".to_string())
        );
    }

    #[test]
    fn writable_only_for_draft() {
        let mut row = sample_row("DRAFT");
        assert!(row.writable());
        for status in ["SUBMITTED", "APPROVED", "SETTLED", "CLOSED", "REVERSED", "CANCELLED"] {
            row.status = status.to_string();
            assert!(!row.writable(), "{status} 不该可写");
        }
    }

    #[test]
    fn totals_summary_is_string_amounts() {
        let totals = PeriodTotals {
            object_count: 2,
            total_material: dec("100.0000"),
            total_labor: dec("20.5000"),
            total_overhead: dec("3.2500"),
            total_cost: dec("123.7500"),
            total_finished: dec("100.0000"),
            total_wip: dec("23.7500"),
        };
        let summary = totals.to_summary();
        assert_eq!(summary.object_count, 2);
        assert_eq!(summary.total_cost, "123.7500");
        assert_eq!(summary.total_wip, "23.7500");
        // 守恒：投入 = 完工 + 在制
        assert_eq!(
            totals.total_cost,
            totals.total_finished + totals.total_wip,
            "明细合计必须幂等守恒（与 ck_biz_cost_item_cons 同口径）"
        );
    }

    fn sample_row(status: &str) -> CostPeriodRow {
        let now = Utc::now();
        CostPeriodRow {
            id: Uuid::from_u128(7),
            org_id: Uuid::nil(),
            period_code: "2026-10".to_string(),
            batch_no: "CS-202610-0001".to_string(),
            status: status.to_string(),
            labor_source: cost_period_dto::LABOR_SOURCE_MANUAL.to_string(),
            object_count: 0,
            total_material: Decimal::ZERO,
            total_labor: Decimal::ZERO,
            total_overhead: Decimal::ZERO,
            total_cost: Decimal::ZERO,
            total_finished: Decimal::ZERO,
            total_wip: Decimal::ZERO,
            approval_instance_id: None,
            labor_voucher_id: None,
            overhead_voucher_id: None,
            finish_voucher_id: None,
            settled_at: None,
            settled_by: None,
            reversed_at: None,
            reversed_by: None,
            reverse_reason: None,
            cancel_reason: None,
            remark: None,
            created_at: now,
            updated_at: now,
            create_by: None,
            update_by: None,
            create_by_name: None,
            settled_by_name: None,
            reversed_by_name: None,
        }
    }
}

//! 成本对象（生产批）数据层（成本核算 · ARCH-9 §3.1 ② / §4.1）。
//!
//! 表结构唯一依据：`backend/migrations/business/V0370__cost_lot_and_cost_object.up.sql`
//! （`biz_cost_object`，与设计 §3.1 ② 逐字一致）。
//!
//! # 名字怎么补（AGENTS 约定 13：按语义分，不一刀切）
//!
//! | 列 | 口径 | 本模块做法 |
//! |---|---|---|
//! | `product_id` → `product_name` / `product_code` | 主数据关联 ⇒ **join 实时取名** | `LEFT JOIN biz_material` |
//! | `workshop_dept_id` → `workshop_dept_name` | 主数据关联 ⇒ **join 实时取名** | `LEFT JOIN sys_dept`，取不到时回落到建单快照 `t.workshop_dept_name` |
//! | `target_warehouse_id` / `target_location_id` | 主数据关联 | `LEFT JOIN biz_warehouse` / `biz_location` |
//! | `customer_id` → `customer_name` | 主数据关联 | `LEFT JOIN biz_customer` |
//! | `workshop_dept_name` 列本身 | **建单时点快照**（审计口径） | 只在上面 join 落空时兜底 |
//!
//! 全部一次 join 拿全，列表里**没有**逐行补名的 N+1 查询。
//!
//! # 数据权限
//!
//! `biz_cost_object` 有车间列 ⇒ 走 `scope::ScopeTarget::new("biz_cost_object", "t",
//! Some("workshop_dept_id"), None)`（登记在 `scope::SCOPED_TABLES`），
//! 列表走 [`scope::paginate`]（架构 §2.5 闸①）。设计 §5.4 点名的就是它。
//! 归属列给 `None`：成本对象是**车间级**对象，没有「本人」语义（SELF 范围由
//! `filter_for` 安全侧降级为仅本人所在部门 —— 与 `biz_commission_settlement` 相反的方向，
//! 那边是人对提成，这边是车间对成本）。
//!
//! # 关批的事实查询为什么放在 model
//!
//! 5 条前置里有 4 条是「查一张别的表」：`biz_cost_labor` / `biz_stock_out` /
//! `biz_cost_period(_item)` / 归集行。判定逻辑在 `cost_object_dto::evaluate_close`（纯函数），
//! 查库在这里 —— 单测可以只喂事实，不用连库。

use std::str::FromStr;

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_object_dto::{
    self, CloseFacts, CostObjectFilter, CostObjectSpec, CostPeriodItemRes, QtyAmount,
};
use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope;

/// 表名。
pub const COST_OBJECT_TABLE: &str = "biz_cost_object";

/// 本模块读的单头列（与 V0370 逐列对应，顺序无关）。
pub const OBJECT_COLUMNS: &str = "id, object_no, object_type, product_id, lot_no, \
     target_warehouse_id, target_location_id, plan_qty, completed_qty, wip_qty, completion_pct, \
     material_input_mode, source_doc_no, customer_id, workshop_dept_id, workshop_dept_name, \
     start_date, due_date, status, closed_at, closed_by, remark, created_at, updated_at";

/// 列表 / 详情的数据权限锚点（车间列 = `workshop_dept_id`）。
pub const OBJECT_TARGET: scope::ScopeTarget =
    scope::ScopeTarget::new(COST_OBJECT_TABLE, "t", Some("workshop_dept_id"), None);

/// 列表全序：在产的排前面，再按开工日期倒序，最后用对象号 / id 收全序
/// （没有全序时翻页会漏行 / 重复行，与其余列表同款）。
pub const OBJECT_ORDER: &str = "ORDER BY (t.status = 'OPEN') DESC, t.start_date DESC, t.object_no DESC, t.id ASC";

// ==================== 行结构 ====================

/// `biz_cost_object` 一行 + join 出来的显示名。
#[derive(Debug, Clone)]
pub struct CostObjectRow {
    /// 主键。
    pub id: Uuid,
    /// 对象号。
    pub object_no: String,
    /// 对象类型。
    pub object_type: String,
    /// 产出品。
    pub product_id: Uuid,
    /// 产出批号。
    pub lot_no: Option<String>,
    /// 目标入库仓。
    pub target_warehouse_id: Uuid,
    /// 目标库位。
    pub target_location_id: Option<Uuid>,
    /// 计划数量。
    pub plan_qty: Decimal,
    /// 累计完工数量。
    pub completed_qty: Decimal,
    /// 期末在制数量。
    pub wip_qty: Decimal,
    /// 期末在制完工进度%。
    pub completion_pct: Decimal,
    /// 投料方式。
    pub material_input_mode: String,
    /// 上游订单号。
    pub source_doc_no: Option<String>,
    /// 客户。
    pub customer_id: Option<Uuid>,
    /// 生产车间。
    pub workshop_dept_id: Uuid,
    /// 车间名快照。
    pub workshop_dept_name: String,
    /// 开工日期。
    pub start_date: NaiveDate,
    /// 计划完工日期。
    pub due_date: Option<NaiveDate>,
    /// 状态。
    pub status: String,
    /// 关批时间。
    pub closed_at: Option<DateTime<Utc>>,
    /// 关批人。
    pub closed_by: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    // ---- join 出来的显示名（NULL 表示主数据已删 / 本来没填） ----
    /// 产出品名称。
    pub product_name: Option<String>,
    /// 产出品编码。
    pub product_code: Option<String>,
    /// 生产车间名称（实时 join 优先，落空回落到快照）。
    pub dept_name: Option<String>,
    /// 目标仓名称。
    pub warehouse_name: Option<String>,
    /// 目标库位名称。
    pub location_name: Option<String>,
    /// 客户名称。
    pub customer_name: Option<String>,
    /// 归集总成本（料 + 工 + 费，列表列 `cost_total`）。
    pub cost_total: Decimal,
}

impl CostObjectRow {
    /// 由查询行构造（列名见 [`OBJECT_COLUMNS`] + join 派生列）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        let snapshot: Option<String> = row.get_typed("workshop_dept_name")?;
        let live: Option<String> = row.get_typed("dept_name")?;
        Ok(Self {
            id: row.get_typed("id")?,
            object_no: row.get_typed("object_no")?,
            object_type: row.get_typed("object_type")?,
            product_id: row.get_typed("product_id")?,
            lot_no: row.get_typed("lot_no")?,
            target_warehouse_id: row.get_typed("target_warehouse_id")?,
            target_location_id: row.get_typed("target_location_id")?,
            plan_qty: row.get_typed("plan_qty")?,
            completed_qty: row.get_typed("completed_qty")?,
            wip_qty: row.get_typed("wip_qty")?,
            completion_pct: row.get_typed("completion_pct")?,
            material_input_mode: row.get_typed("material_input_mode")?,
            source_doc_no: row.get_typed("source_doc_no")?,
            customer_id: row.get_typed("customer_id")?,
            workshop_dept_id: row.get_typed("workshop_dept_id")?,
            workshop_dept_name: snapshot.clone().unwrap_or_default(),
            start_date: row.get_typed("start_date")?,
            due_date: row.get_typed("due_date")?,
            status: row.get_typed("status")?,
            closed_at: row.get_typed("closed_at")?,
            closed_by: row.get_typed("closed_by")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            product_name: row.get_typed("product_name")?,
            product_code: row.get_typed("product_code")?,
            // 实时名优先，落空回落到建单快照（快照 NOT NULL，一定读得到）
            dept_name: live.or(snapshot),
            warehouse_name: row.get_typed("warehouse_name")?,
            location_name: row.get_typed("location_name")?,
            customer_name: row.get_typed("customer_name")?,
            cost_total: row.get_typed("cost_total")?,
        })
    }

    /// 还原成归一化规格（编辑时作为「当前值」参与合并）。
    pub fn to_spec(&self) -> CostObjectSpec {
        CostObjectSpec {
            object_type: self.object_type.clone(),
            product_id: self.product_id,
            lot_no: self.lot_no.clone(),
            target_warehouse_id: self.target_warehouse_id,
            target_location_id: self.target_location_id,
            plan_qty: self.plan_qty,
            workshop_dept_id: self.workshop_dept_id,
            workshop_dept_name: self.workshop_dept_name.clone(),
            start_date: self.start_date,
            due_date: self.due_date,
            source_doc_no: self.source_doc_no.clone(),
            customer_id: self.customer_id,
            material_input_mode: self.material_input_mode.clone(),
            remark: self.remark.clone(),
        }
    }

    /// 车间显示名（join 不到就空）。
    pub fn dept_display(&self) -> Option<String> {
        self.dept_name.clone().filter(|text| !text.is_empty())
    }
}

// ==================== 读 ====================

/// 单头 + 显示名 + 归集总成本的公共 SELECT。
///
/// 归集总成本用**相关子查询**（`Σ amount`）而不是 GROUP BY：一个对象最多几百行归集，
/// 相关子查询走 `idx_biz_cost_collect_obj` 索引，且不会把 join 结果行数放大
/// （用 LEFT JOIN + GROUP BY 时，`biz_cost_collect` 每多一行就多一行单头，容易漏 DISTINCT）。
fn select_sql() -> String {
    let columns: Vec<String> = OBJECT_COLUMNS
        .split(',')
        .map(|column| format!("t.{}", column.trim()))
        .collect();
    format!(
        "SELECT {}, \
                m.material_name AS product_name, \
                m.material_code AS product_code, \
                d.dept_name     AS dept_name, \
                w.warehouse_name AS warehouse_name, \
                l.location_name  AS location_name, \
                c.name           AS customer_name, \
                COALESCE((SELECT sum(cc.amount) FROM biz_cost_collect cc \
                           WHERE cc.cost_object_id = t.id AND cc.deleted_at IS NULL), 0) AS cost_total \
           FROM {COST_OBJECT_TABLE} t \
           LEFT JOIN biz_material m ON m.id = t.product_id \
           LEFT JOIN sys_dept d     ON d.id = t.workshop_dept_id AND d.deleted_at IS NULL \
           LEFT JOIN biz_warehouse w ON w.id = t.target_warehouse_id \
           LEFT JOIN biz_location l  ON l.id = t.target_location_id \
           LEFT JOIN biz_customer c  ON c.id = t.customer_id",
        columns.join(", ")
    )
}

/// 按主键取存活对象（软删 / 不存在 → `None`）。
pub async fn find_by_id(id: Uuid) -> DbResult<Option<CostObjectRow>> {
    let sql = format!("{} WHERE t.id = ?::uuid AND t.deleted_at IS NULL", select_sql());
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(CostObjectRow::from_row).transpose()
}

/// 列表分页（数据权限按车间裁剪）。
///
/// `{FILTER}` 由 `scope::paginate` 原位替换：本函数的 SQL 里业务条件写在 SELECT 之后，
/// 因此片段参数恒前置（架构 §2.6）—— `scope::paginate` 自己拼片段，这里不用写标记。
pub async fn page(principal: &ScopedPrincipal, filter: &CostObjectFilter) -> DbResult<(Vec<CostObjectRow>, i64)> {
    let mut sql = format!("{} WHERE t.deleted_at IS NULL", select_sql());
    let mut params: Vec<Value> = Vec::new();
    if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND t.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(product_id) = filter.product_id {
        sql.push_str(" AND t.product_id = ?::uuid");
        params.push(Value::from(&product_id));
    }
    if let Some(dept_id) = filter.workshop_dept_id {
        sql.push_str(" AND t.workshop_dept_id = ?::uuid");
        params.push(Value::from(&dept_id));
    }
    if let Some(from) = filter.start_from {
        sql.push_str(" AND t.start_date >= ?::text::date");
        params.push(Value::Text(from.to_string()));
    }
    if let Some(to) = filter.start_to {
        sql.push_str(" AND t.start_date <= ?::text::date");
        params.push(Value::Text(to.to_string()));
    }
    if let Some(pattern) = utils_like(&filter.keyword) {
        sql.push_str(
            " AND (t.object_no LIKE ? OR COALESCE(t.lot_no, '') LIKE ? OR COALESCE(t.source_doc_no, '') LIKE ?)",
        );
        for _ in 0..3 {
            params.push(Value::from(pattern.clone()));
        }
    }
    let result = scope::paginate(
        OBJECT_TARGET,
        &sql,
        params,
        Some(OBJECT_ORDER),
        filter.page,
        filter.page_size,
        principal,
    )
    .await?;
    let rows = result
        .list
        .iter()
        .map(CostObjectRow::from_row)
        .collect::<DbResult<Vec<CostObjectRow>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 关键字 → LIKE 模式（与 `utils::like_pattern` 同口径，做成小函数便于单测）。
fn utils_like(keyword: &Option<String>) -> Option<String> {
    keyword
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| format!("%{}%", text.replace('%', "\\%").replace('_', "\\_")))
}

/// 归集汇总（料 / 工 / 费三项；`biz_cost_collect.element` 分组）。
pub async fn collect_summary(cost_object_id: Uuid) -> DbResult<(Decimal, Decimal, Decimal)> {
    let rows = db()
        .query_sql(
            "SELECT element, COALESCE(sum(amount), 0) AS amount \
               FROM biz_cost_collect \
              WHERE cost_object_id = ?::uuid AND deleted_at IS NULL \
              GROUP BY element"
                .to_string(),
            vec![Value::from(&cost_object_id)],
        )
        .await?;
    let mut mat = Decimal::ZERO;
    let mut labor = Decimal::ZERO;
    let mut oh = Decimal::ZERO;
    for row in &rows {
        let element: String = row.get_typed("element")?;
        let amount: Decimal = row.get_typed("amount")?;
        match element.as_str() {
            "MATERIAL" => mat = amount,
            "LABOR" => labor = amount,
            "OVERHEAD" => oh = amount,
            _ => {}
        }
    }
    Ok((mat, labor, oh))
}

/// 成本计算单（该对象在全期间的 `biz_cost_period_item`，按期倒序）。
pub async fn period_items(cost_object_id: Uuid) -> DbResult<Vec<CostPeriodItemRes>> {
    let rows = db()
        .query_sql(
            "SELECT p.period_code, p.status AS period_status, i.status, \
                    i.wip_beg_qty, i.wip_beg_cost, i.completed_qty, i.wip_end_qty, i.completion_pct, \
                    i.equiv_material, i.equiv_conversion, i.mat_cur, i.labor_cur, i.oh_cur, \
                    i.unit_material, i.unit_labor, i.unit_overhead, i.finished_cost, i.wip_end_cost, \
                    i.unit_finished_cost, i.stock_in_id \
               FROM biz_cost_period_item i \
               JOIN biz_cost_period p ON p.id = i.period_id \
              WHERE i.cost_object_id = ?::uuid AND p.deleted_at IS NULL \
              ORDER BY p.period_code DESC"
                .to_string(),
            vec![Value::from(&cost_object_id)],
        )
        .await?;
    let mut items = Vec::with_capacity(rows.len());
    for row in &rows {
        let stock_in_id: Option<Uuid> = row.get_typed("stock_in_id")?;
        items.push(CostPeriodItemRes {
            period_code: row.get_typed("period_code")?,
            period_status: row.get_typed("period_status")?,
            status: row.get_typed("status")?,
            wip_beg_qty: amount_text(row.get_typed("wip_beg_qty")?),
            wip_beg_cost: amount_text(row.get_typed("wip_beg_cost")?),
            completed_qty: amount_text(row.get_typed("completed_qty")?),
            wip_end_qty: amount_text(row.get_typed("wip_end_qty")?),
            completion_pct: amount_text(row.get_typed("completion_pct")?),
            equiv_material: amount_text(row.get_typed("equiv_material")?),
            equiv_conversion: amount_text(row.get_typed("equiv_conversion")?),
            mat_cur: amount_text(row.get_typed("mat_cur")?),
            labor_cur: amount_text(row.get_typed("labor_cur")?),
            oh_cur: amount_text(row.get_typed("oh_cur")?),
            unit_material: amount_text(row.get_typed("unit_material")?),
            unit_labor: amount_text(row.get_typed("unit_labor")?),
            unit_overhead: amount_text(row.get_typed("unit_overhead")?),
            finished_cost: amount_text(row.get_typed("finished_cost")?),
            wip_end_cost: amount_text(row.get_typed("wip_end_cost")?),
            unit_finished_cost: amount_text(row.get_typed("unit_finished_cost")?),
            stock_in_id: stock_in_id.map(|id| id.to_string()),
        });
    }
    Ok(items)
}

/// `Decimal` → 字符串（金额 / 数量统一字符串出参）。
fn amount_text(value: Decimal) -> String {
    value.to_string()
}

// ==================== 反查：领料 / 完工入库（B4-2 · §8.1） ====================
//
// 详情页「这个批的料从哪来、货去哪了」的两条反查链路：
//
// ```text
// 料  biz_stock_out (cost_object_id = ?) → biz_stock_out_item（数量 / 金额）
//      → 已归集部分（biz_cost_collect.source_id = 明细 id，含归集期间）
// 货  biz_stock_in (cost_object_id = ? 且 in_type='PRODUCTION')
//      → biz_stock_in_item（数量 / 金额）→ 结算批次（biz_cost_period.batch_no）
// ```
//
// # 三个口径
//
// 1. **不读单头合计**：`total_qty / total_amount` 是过账 / 结算那一刻的快照，
//    明细被改 / 单据被作废时单头会滞后。这里逐行 `SUM(明细)`，合计才等于「表格里各行相加」；
// 2. **作废单要列出来**（`status='CANCELLED'`）：红冲把完工入库单打成 CANCELLED，
//    页面上必须能看到「这批货被红冲了」，只是不计入合计（判据与它同源，见 dto）；
// 3. **子查询各自聚合再 join**：明细与归集行是 1:N 的两张表，直接 join 会把金额乘起来
//    （一张 3 行的单 × 3 条归集行 ⇒ 金额×3）。两个 `GROUP BY` 子查询各自汇总后按单据
//    id join，才不会被笛卡尔积污染。

/// 一条领料单（`biz_stock_out` + 明细 + 归集）的反查行。
#[derive(Debug, Clone)]
pub struct IssueDocRow {
    /// `biz_stock_out.id`。
    pub id: Uuid,
    /// 出库单号。
    pub doc_no: String,
    /// 出库日期（决定归集期间）。
    pub doc_date: NaiveDate,
    /// 单据状态 `DRAFT / PENDING_POST / POSTED / CANCELLED`。
    pub status: String,
    /// 仓库名（实时 join，主数据改名后显示新名 —— 约定 13）。
    pub warehouse_name: Option<String>,
    /// 出库事由 / 备注。
    pub remark: Option<String>,
    /// 过账凭证。
    pub voucher_id: Option<Uuid>,
    /// 过账凭证号。
    pub voucher_no: Option<String>,
    /// 明细数量合计。
    pub qty: Decimal,
    /// 明细金额合计。
    pub amount: Decimal,
    /// 明细行数。
    pub line_count: i64,
    /// 已归集数量（`biz_cost_collect.qty` 合计）。
    pub collected_qty: Decimal,
    /// 已归集金额。
    pub collected_amount: Decimal,
    /// 归集期间（跨期补录时可能有两个）。
    pub period_codes: Vec<String>,
}

/// 一条完工入库单（`biz_stock_in`，`in_type='PRODUCTION'`）的反查行。
#[derive(Debug, Clone)]
pub struct ReceiptDocRow {
    /// `biz_stock_in.id`。
    pub id: Uuid,
    /// 入库单号。
    pub doc_no: String,
    /// 入库日期。
    pub doc_date: NaiveDate,
    /// 单据状态 `POSTED / CANCELLED`（红冲）。
    pub status: String,
    /// 仓库名。
    pub warehouse_name: Option<String>,
    /// 备注（结算写入的「完工入库 · 结算批次 … · 成本对象 …」）。
    pub remark: Option<String>,
    /// 完工结转凭证（结算第 3 张）。
    pub voucher_id: Option<Uuid>,
    /// 凭证号。
    pub voucher_no: Option<String>,
    /// 结算批次号 `CS-yyyyMM-0001`。
    pub batch_no: Option<String>,
    /// 结算期间 `yyyy-MM`。
    pub period_code: Option<String>,
    /// 明细数量合计。
    pub qty: Decimal,
    /// 明细金额合计。
    pub amount: Decimal,
    /// 明细行数。
    pub line_count: i64,
}

/// 该对象名下的领料单（全部状态，按出库日期升序）。
pub async fn source_issues(cost_object_id: Uuid) -> DbResult<Vec<IssueDocRow>> {
    let rows = db()
        .query_sql(
            "SELECT h.id, h.out_no, h.out_date, h.status, h.reason, h.voucher_id, \
                    w.warehouse_name, v.voucher_no, \
                    COALESCE(it.qty, 0) AS qty, COALESCE(it.amount, 0) AS amount, \
                    COALESCE(it.line_count, 0) AS line_count, \
                    COALESCE(cc.collected_qty, 0) AS collected_qty, \
                    COALESCE(cc.collected_amount, 0) AS collected_amount, \
                    COALESCE(cc.period_codes, '') AS period_codes \
               FROM biz_stock_out h \
               LEFT JOIN biz_warehouse w ON w.id = h.warehouse_id \
               LEFT JOIN biz_voucher v ON v.id = h.voucher_id \
               LEFT JOIN (SELECT stock_out_id, sum(qty) AS qty, sum(amount) AS amount, \
                                 count(*) AS line_count \
                            FROM biz_stock_out_item GROUP BY stock_out_id) it \
                      ON it.stock_out_id = h.id \
               LEFT JOIN (SELECT soi.stock_out_id AS stock_out_id, \
                                 sum(c.qty) AS collected_qty, sum(c.amount) AS collected_amount, \
                                 string_agg(DISTINCT c.period_code, ',') AS period_codes \
                            FROM biz_cost_collect c \
                            JOIN biz_stock_out_item soi ON soi.id = c.source_id \
                           WHERE c.cost_object_id = ?::uuid AND c.element = 'MATERIAL' \
                             AND c.source_type = 'STOCK_OUT' AND c.deleted_at IS NULL \
                           GROUP BY soi.stock_out_id) cc \
                      ON cc.stock_out_id = h.id \
              WHERE h.cost_object_id = ?::uuid AND h.deleted_at IS NULL \
              ORDER BY h.out_date ASC, h.out_no ASC, h.id ASC"
                .to_string(),
            vec![Value::from(&cost_object_id), Value::from(&cost_object_id)],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let period_text: String = row.get_typed("period_codes")?;
        out.push(IssueDocRow {
            id: row.get_typed("id")?,
            doc_no: row.get_typed("out_no")?,
            doc_date: row.get_typed("out_date")?,
            status: row.get_typed("status")?,
            warehouse_name: row.get_typed("warehouse_name")?,
            remark: row.get_typed("reason")?,
            voucher_id: row.get_typed("voucher_id")?,
            voucher_no: row.get_typed("voucher_no")?,
            qty: row.get_typed("qty")?,
            amount: row.get_typed("amount")?,
            line_count: row.get_typed("line_count")?,
            collected_qty: row.get_typed("collected_qty")?,
            collected_amount: row.get_typed("collected_amount")?,
            period_codes: split_periods(&period_text),
        });
    }
    Ok(out)
}

/// 该对象名下的完工入库单（`in_type='PRODUCTION'`，含被红冲作废的）。
pub async fn source_receipts(cost_object_id: Uuid) -> DbResult<Vec<ReceiptDocRow>> {
    let rows = db()
        .query_sql(
            "SELECT h.id, h.in_no, h.in_date, h.status, h.remark, h.voucher_id, \
                    w.warehouse_name, v.voucher_no, p.batch_no, p.period_code, \
                    COALESCE(it.qty, 0) AS qty, COALESCE(it.amount, 0) AS amount, \
                    COALESCE(it.line_count, 0) AS line_count \
               FROM biz_stock_in h \
               LEFT JOIN biz_warehouse w ON w.id = h.warehouse_id \
               LEFT JOIN biz_voucher v ON v.id = h.voucher_id \
               LEFT JOIN biz_cost_period p ON p.id = h.cost_period_id AND p.deleted_at IS NULL \
               LEFT JOIN (SELECT stock_in_id, sum(qty) AS qty, sum(amount) AS amount, \
                                 count(*) AS line_count \
                            FROM biz_stock_in_item GROUP BY stock_in_id) it \
                      ON it.stock_in_id = h.id \
              WHERE h.cost_object_id = ?::uuid AND h.in_type = 'PRODUCTION' \
                AND h.deleted_at IS NULL \
              ORDER BY h.in_date ASC, h.in_no ASC, h.id ASC"
                .to_string(),
            vec![Value::from(&cost_object_id)],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(ReceiptDocRow {
            id: row.get_typed("id")?,
            doc_no: row.get_typed("in_no")?,
            doc_date: row.get_typed("in_date")?,
            status: row.get_typed("status")?,
            warehouse_name: row.get_typed("warehouse_name")?,
            remark: row.get_typed("remark")?,
            voucher_id: row.get_typed("voucher_id")?,
            voucher_no: row.get_typed("voucher_no")?,
            batch_no: row.get_typed("batch_no")?,
            period_code: row.get_typed("period_code")?,
            qty: row.get_typed("qty")?,
            amount: row.get_typed("amount")?,
            line_count: row.get_typed("line_count")?,
        });
    }
    Ok(out)
}

/// 该对象**已归集**的料行合计（`source_type='STOCK_OUT'`；MANUAL 料行不算领料链）。
pub async fn collected_material_of(cost_object_id: Uuid) -> DbResult<QtyAmount> {
    let rows = db()
        .query_sql(
            "SELECT COALESCE(sum(qty), 0) AS qty, COALESCE(sum(amount), 0) AS amount \
               FROM biz_cost_collect \
              WHERE cost_object_id = ?::uuid AND element = 'MATERIAL' \
                AND source_type = 'STOCK_OUT' AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&cost_object_id)],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(QtyAmount::new(row.get_typed("qty")?, row.get_typed("amount")?)),
        // SUM 一定回一行；零行只可能是驱动异常，按 0 处理不拦页面（判据会把差额显示出来）
        None => Ok(QtyAmount::default()),
    }
}

/// 该对象**已结算**计算单的累计完工数量 / 完工转出成本合计。
///
/// 只看 `status='SETTLED'`：红冲把行打回 `CALC`，此时它与被作废的入库单**同时**退出合计。
pub async fn settled_output_of(cost_object_id: Uuid) -> DbResult<QtyAmount> {
    let rows = db()
        .query_sql(
            "SELECT COALESCE(sum(i.completed_qty), 0) AS qty, \
                    COALESCE(sum(i.finished_cost), 0) AS amount \
               FROM biz_cost_period_item i \
               JOIN biz_cost_period p ON p.id = i.period_id \
              WHERE i.cost_object_id = ?::uuid AND i.status = 'SETTLED' AND p.deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&cost_object_id)],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(QtyAmount::new(row.get_typed("qty")?, row.get_typed("amount")?)),
        None => Ok(QtyAmount::default()),
    }
}

/// `string_agg` 的期间串 → 期间列表（去空、保序）。
fn split_periods(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .collect()
}

// ==================== 引用存在性（建 / 改单的查库校验） ====================

/// 产出品是否可用：存在、未软删、且 `material_type IN ('FINISHED','SEMI')`。
///
/// 设计 §6.2.1 只说「物料选择器只列 FINISHED / SEMI」，后端不能靠前端自觉 ——
/// 挂一个 `RAW` 原料当成本对象，结算时「完工入库」会入一笔原料，库存口径直接错。
pub async fn finished_product_exists(product_id: Uuid) -> DbResult<bool> {
    let count: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_material \
              WHERE id = ?::uuid AND deleted_at IS NULL AND material_type IN ('FINISHED','SEMI')",
            vec![Value::from(&product_id)],
        )
        .await?;
    Ok(count > 0)
}

/// 仓库是否存在且未软删（返回 `(true, 名称)`）。
pub async fn warehouse_of(warehouse_id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .query_sql(
            "SELECT warehouse_name FROM biz_warehouse WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(&warehouse_id)],
        )
        .await?;
    rows.first().map(|row| row.get_typed("warehouse_name")).transpose()
}

/// 库位是否存在、未软删、且**属于给定仓库**（防「仓库 A 的库位挂到仓库 B」）。
pub async fn location_of(location_id: Uuid, warehouse_id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .query_sql(
            "SELECT location_name FROM biz_location \
              WHERE id = ?::uuid AND warehouse_id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&location_id), Value::from(&warehouse_id)],
        )
        .await?;
    rows.first().map(|row| row.get_typed("location_name")).transpose()
}

/// 部门是否存在（返回名称，用作车间名快照）。
pub async fn dept_name_of(dept_id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .query_sql(
            "SELECT dept_name FROM sys_dept WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(&dept_id)],
        )
        .await?;
    rows.first().map(|row| row.get_typed("dept_name")).transpose()
}

/// 客户是否存在且未软删。
pub async fn customer_exists(customer_id: Uuid) -> DbResult<bool> {
    let count: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_customer WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&customer_id)],
        )
        .await?;
    Ok(count > 0)
}

/// 该对象**已经有多少行归集**（`product_id` 改动的守卫：有归集就只读）。
pub async fn collect_row_count(cost_object_id: Uuid) -> DbResult<i64> {
    db().scalar_with_params(
        "SELECT count(*) FROM biz_cost_collect WHERE cost_object_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&cost_object_id)],
    )
    .await
}

// ==================== 写 ====================

/// 建单（`object_no` 已由 handler 取号；`workshop_dept_name` 已由 handler 填快照）。
pub async fn insert(id: Uuid, object_no: &str, spec: &CostObjectSpec, operator: Option<Uuid>) -> DbResult<u64> {
    db().exec_sql(
        "INSERT INTO biz_cost_object \
             (id, object_no, object_type, product_id, lot_no, target_warehouse_id, target_location_id, \
              plan_qty, completed_qty, wip_qty, completion_pct, material_input_mode, source_doc_no, \
              customer_id, workshop_dept_id, workshop_dept_name, start_date, due_date, status, \
              remark, created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?, ?, ?::uuid, NULLIF(?::text,''), ?::uuid, NULLIF(?::text,'')::uuid, \
                 ?::text::numeric, 0, 0, 0, ?, NULLIF(?::text,''), NULLIF(?::text,'')::uuid, \
                 ?::uuid, ?, ?::text::date, NULLIF(?::text,'')::date, 'OPEN', \
                 NULLIF(?::text,''), now(), now(), NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
            .to_string(),
        vec![
            Value::from(&id),
            Value::Text(object_no.to_string()),
            Value::Text(spec.object_type.clone()),
            Value::from(&spec.product_id),
            Value::Text(spec.lot_no.clone().unwrap_or_default()),
            Value::from(&spec.target_warehouse_id),
            Value::Text(spec.target_location_id.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(spec.plan_qty.to_string()),
            Value::Text(spec.material_input_mode.clone()),
            Value::Text(spec.source_doc_no.clone().unwrap_or_default()),
            Value::Text(spec.customer_id.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&spec.workshop_dept_id),
            Value::Text(spec.workshop_dept_name.clone()),
            Value::Text(spec.start_date.to_string()),
            Value::Text(spec.due_date.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(spec.remark.clone().unwrap_or_default()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
        ],
    )
    .await
}

/// 覆盖写业务字段（**只在 `OPEN` 且未关批时**调用；CAS 在 SQL 里再兜一层）。
///
/// `WHERE status = 'OPEN'` 是死单防线：两个人同时点保存 / 关批时，后到的那条
/// `affected = 0`，handler 转 409 而不是把已关批的单改回在产。
pub async fn update(id: Uuid, spec: &CostObjectSpec, operator: Option<Uuid>) -> DbResult<u64> {
    db().exec_sql(
        "UPDATE biz_cost_object SET \
             product_id = ?::uuid, lot_no = NULLIF(?::text,''), \
             target_warehouse_id = ?::uuid, target_location_id = NULLIF(?::text,'')::uuid, \
             plan_qty = ?::text::numeric, workshop_dept_id = ?::uuid, workshop_dept_name = ?, \
             start_date = ?::text::date, due_date = NULLIF(?::text,'')::date, \
             source_doc_no = NULLIF(?::text,''), customer_id = NULLIF(?::text,'')::uuid, \
             material_input_mode = ?, remark = NULLIF(?::text,''), \
             updated_at = now(), update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL AND status = 'OPEN'"
            .to_string(),
        vec![
            Value::from(&spec.product_id),
            Value::Text(spec.lot_no.clone().unwrap_or_default()),
            Value::from(&spec.target_warehouse_id),
            Value::Text(spec.target_location_id.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(spec.plan_qty.to_string()),
            Value::from(&spec.workshop_dept_id),
            Value::Text(spec.workshop_dept_name.clone()),
            Value::Text(spec.start_date.to_string()),
            Value::Text(spec.due_date.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(spec.source_doc_no.clone().unwrap_or_default()),
            Value::Text(spec.customer_id.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(spec.material_input_mode.clone()),
            Value::Text(spec.remark.clone().unwrap_or_default()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

/// 关批落库（CAS：`WHERE status = 'OPEN'`；`closed_at` 必填 —— 库层 CHECK 也要求）。
pub async fn apply_close(id: Uuid, operator: Option<Uuid>, remark: Option<&str>, now: DateTime<Utc>) -> DbResult<u64> {
    db().exec_sql(
        "UPDATE biz_cost_object SET status = 'CLOSED', closed_at = ?, \
             closed_by = NULLIF(?::text,'')::uuid, \
             remark = CASE WHEN ? = '' THEN remark \
                           ELSE COALESCE(remark || E'\\n', '') || ? END, \
             updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL AND status = 'OPEN'"
            .to_string(),
        vec![
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(remark.unwrap_or("").to_string()),
            Value::Text(remark.unwrap_or("").to_string()),
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

/// 批量更新完工 / 在制数量（结算回写；本卡不调用，留给 B3 结算段）。
///
/// 放在这里是因为「谁写这张表」要单点：结算段不该再各写一条 UPDATE
/// （两份口径迟早漂开，例如一边改 `completion_pct` 一边不改）。
pub async fn apply_production(
    id: Uuid,
    completed_qty: Decimal,
    wip_qty: Decimal,
    completion_pct: Decimal,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    db().exec_sql(
        "UPDATE biz_cost_object SET completed_qty = ?::text::numeric, wip_qty = ?::text::numeric, \
             completion_pct = ?::text::numeric, updated_at = now(), \
             update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::Text(completed_qty.to_string()),
            Value::Text(wip_qty.to_string()),
            Value::Text(completion_pct.to_string()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

// ==================== 本期产量与在制（B2-14） ====================

/// 该对象**已结算**的期间（`period_code`, `status`）。
///
/// # 口径（卡面逐字）
///
/// `biz_cost_period.status ∈ {SETTLED, CLOSED}` **且该对象有计算单** ⇒ 这些期间里的
/// 产量/在制已经变成入库金额与结转凭证（§4.7.4），再改就是改写已入账成本。
///
/// 为什么要求「有计算单」：期间结算了但这批对象没参与（没投入也没产量）时，
/// 它的产量数字与任何账务无关，仍允许补录。
///
/// # SCOPE-EXEMPT
///
/// 按对象主键回查它参与过的期间（单个对象、非列表 / 导出），不涉及行级可见性。
// SCOPE-EXEMPT: 按 cost_object_id 回查已结算期间（单对象只读校验，非列表查询）
pub async fn settled_periods(cost_object_id: Uuid) -> DbResult<Vec<(String, String)>> {
    let rows = db()
        .query_sql(
            "SELECT p.period_code, p.status \
               FROM biz_cost_period p \
              WHERE p.deleted_at IS NULL AND p.status IN ('SETTLED','CLOSED') \
                AND EXISTS (SELECT 1 FROM biz_cost_period_item i \
                             WHERE i.cost_object_id = ?::uuid AND i.period_id = p.id) \
              ORDER BY p.period_code DESC"
                .to_string(),
            vec![Value::from(&cost_object_id)],
        )
        .await?;
    let mut periods = Vec::with_capacity(rows.len());
    for row in &rows {
        periods.push((row.get_typed("period_code")?, row.get_typed("status")?));
    }
    Ok(periods)
}

/// 写「本期产量与在制」（CAS：`WHERE status = 'OPEN'`）。
///
/// # 为什么单独一条 SQL 而不改 [`apply_production`]
///
/// `apply_production` 是**结算回写**用的（B3 段，结算在事务里已确认对象是 OPEN），
/// 它没有状态守卫；产量录入是**人工写**，必须自带 `status='OPEN'` 守卫 ——
/// 两个人在「保存产量」与「关批」之间抢跑时，后到的那条 `affected = 0`，
/// handler 转 409 而不是把已关批的单改回可写。
///
/// 参数顺序与 SQL 占位符一一对应（`?::text::numeric` 保证按字符串传来也不丢标度）。
pub async fn apply_output(id: Uuid, spec: &cost_object_dto::OutputSpec, operator: Option<Uuid>) -> DbResult<u64> {
    db().exec_sql(
        "UPDATE biz_cost_object SET completed_qty = ?::text::numeric, wip_qty = ?::text::numeric, \
             completion_pct = ?::text::numeric, updated_at = now(), \
             update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL AND status = 'OPEN'"
            .to_string(),
        vec![
            Value::Text(spec.completed_qty.to_string()),
            Value::Text(spec.wip_qty.to_string()),
            Value::Text(spec.completion_pct.to_string()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

/// 最近一次「填写本期产量与在制」的**填写人 + 时间**（设计 §8.3 R-4）。
///
/// 来源是变更日志（`sys_change_log.operator_name` 快照 + `created_at`），
/// **不加表上三列快照**（§4.6.3 末段的一期口径）。按 `action_label` 过滤，
/// 将来改主数据也接审计时不会把「改备注」显示成「填产量」。
///
/// # SCOPE-EXEMPT
///
/// 按对象主键取审计行（单对象只读，非列表 / 导出）。
// SCOPE-EXEMPT: 按 resource_id 取最近一条产量填写审计（单对象只读，非列表查询）
pub async fn latest_output_fill(id: Uuid) -> DbResult<Option<(Option<String>, DateTime<Utc>)>> {
    crate::business::changelog::model::latest_for_object(
        db(),
        COST_OBJECT_TABLE,
        &id.to_string(),
        Some(cost_object_dto::OUTPUT_ACTION_LABEL),
    )
    .await
}

/// 建批次主数据（`lot_no` 有值时的副作用；`source_type=PRODUCTION`）。
///
/// 唯一键 `uk_biz_lot_material_no (material_id, lot_no) WHERE deleted_at IS NULL`
/// ⇒ 用 `ON CONFLICT DO NOTHING`：同一个批号重复提交（改单再存一次）不该 500，
/// 也不该覆盖别人建的那一行（批次主数据是共享档案）。
pub async fn ensure_production_lot(
    lot_no: &str,
    material_id: Uuid,
    source_doc_no: Option<&str>,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    db().exec_sql(
        "INSERT INTO biz_lot (id, lot_no, material_id, source_type, source_doc_type, \
                              source_doc_no, status, remark, created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?, ?::uuid, 'PRODUCTION', 'COST_OBJECT', NULLIF(?::text,''), 1, \
                 '成本对象建批时自动创建', now(), now(), NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid) \
         ON CONFLICT (material_id, lot_no) WHERE deleted_at IS NULL DO NOTHING"
            .to_string(),
        vec![
            Value::from(&Uuid::now_v7()),
            Value::Text(lot_no.to_string()),
            Value::from(&material_id),
            Value::Text(source_doc_no.unwrap_or("").to_string()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
        ],
    )
    .await
}

// ==================== 关批：事实查询 ====================

/// 采集关批 5 条前置需要的事实（4 条查库 + `wip_qty` / `force_wip` 由调用方给）。
///
/// 每条 SQL 的口径都写在下面对应的注释里；判定逻辑在
/// [`cost_object_dto::evaluate_close`]（纯函数，单测直接喂 `CloseFacts`）。
pub async fn close_facts(id: Uuid, wip_qty: Decimal, force_wip: bool) -> DbResult<CloseFacts> {
    // ① 草稿工时台账：`biz_cost_labor.status='DRAFT'`（表由 V0371 建）
    let draft_labor: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_cost_labor \
              WHERE cost_object_id = ?::uuid AND status = 'DRAFT' AND deleted_at IS NULL",
            vec![Value::from(&id)],
        )
        .await?;

    // ② 已过账但未归集的领料出库：POSTED 且**没有**对应的归集行
    //    （归集行的幂等键是 `(source_type='STOCK_OUT', source_id=出库单 id, element='MATERIAL')`）。
    //    期间已在 SETTLED / CLOSED 批次内的不算（那些已经结算，缺归集是历史既成事实，
    //    拦关批也补不回来 —— 与设计 §4.1 前置 ②「且期间不在已结算批次内」一致）。
    let uncollected_stock_out: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_stock_out so \
              WHERE so.cost_object_id = ?::uuid AND so.deleted_at IS NULL AND so.status = 'POSTED' \
                AND NOT EXISTS (SELECT 1 FROM biz_cost_collect cc \
                                 WHERE cc.source_type = 'STOCK_OUT' AND cc.source_id = so.id \
                                   AND cc.element = 'MATERIAL' AND cc.deleted_at IS NULL) \
                AND NOT EXISTS (SELECT 1 FROM biz_cost_period p \
                                 WHERE p.status IN ('SETTLED','CLOSED') AND p.deleted_at IS NULL \
                                   AND p.period_code = to_char(so.out_date, 'YYYY-MM'))",
            vec![Value::from(&id)],
        )
        .await?;

    // ③ 该对象有活动、且成本批次已锁定（状态 ∉ {DRAFT,SUBMITTED}）的期间。
    //    「有活动」= 有归集行 或 有计算单行（两者都是「这个对象的成本已经进过这条船」）。
    let locked_rows = db()
        .query_sql(
            "SELECT DISTINCT p.period_code, p.status \
               FROM biz_cost_period p \
              WHERE p.deleted_at IS NULL AND p.status NOT IN ('DRAFT','SUBMITTED','CANCELLED','REVERSED') \
                AND (EXISTS (SELECT 1 FROM biz_cost_collect cc \
                              WHERE cc.cost_object_id = ?::uuid AND cc.deleted_at IS NULL \
                                AND cc.period_code = p.period_code) \
                  OR EXISTS (SELECT 1 FROM biz_cost_period_item i \
                              WHERE i.cost_object_id = ?::uuid AND i.period_id = p.id)) \
              ORDER BY p.period_code DESC"
                .to_string(),
            vec![Value::from(&id), Value::from(&id)],
        )
        .await?;
    let mut locked_periods = Vec::with_capacity(locked_rows.len());
    for row in &locked_rows {
        locked_periods.push((row.get_typed("period_code")?, row.get_typed("status")?));
    }

    // ④ 未过账（草稿 / 待审）却挂着该对象的领料出库单
    let unposted_stock_out: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_stock_out \
              WHERE cost_object_id = ?::uuid AND deleted_at IS NULL \
                AND status IN ('DRAFT','PENDING_POST')",
            vec![Value::from(&id)],
        )
        .await?;

    Ok(CloseFacts {
        draft_labor,
        uncollected_stock_out,
        locked_periods,
        unposted_stock_out,
        wip_qty,
        force_wip,
    })
}

// ==================== 状态机 ====================

/// 成本对象的合法边（与 `state_machine::COST_OBJECT_EDGES` 逐条一致，顺序也一致）。
///
/// ```text
/// OPEN ──关批──▶ CLOSED
/// OPEN ──作废──▶ CANCELLED
/// ```
///
/// `CLOSED` 不可回 `OPEN`（设计 §4.1：成本已结转，重开会破坏已结算期间的归集基线；
/// 要复工就新建批）。
pub const LEGAL_EDGES: [(&str, &str); 2] = [
    (cost_object_dto::STATUS_OPEN, cost_object_dto::STATUS_CLOSED),
    (cost_object_dto::STATUS_OPEN, cost_object_dto::STATUS_CANCELLED),
];

/// 有效边（`BASE_EDGES ∩ biz_flow_state_edge` 配置）。
pub async fn effective_edges() -> Vec<(String, String)> {
    crate::business::shared::state_machine::effective_edges(
        db(),
        crate::business::shared::state_machine::Entity::CostObject,
    )
    .await
    .into_iter()
    .map(|edge| (edge.from.to_string(), edge.to.to_string()))
    .collect()
}

/// 解析数量字符串（`plan_qty` 等入参在 DTO 里是 `Decimal`，这里给测试 / 回写用）。
pub fn parse_qty(raw: &str) -> DbResult<Decimal> {
    Decimal::from_str(raw.trim()).map_err(|error| DbError::InvalidValue(format!("数量「{raw}」不合法：{error}")))
}

// ==================== 单测（纯逻辑：不需要库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::shared::state_machine;

    #[test]
    fn legal_edges_match_state_machine_registry() {
        // 边表只有一份真相（`state_machine`），本模块的常量必须逐条一致 —— 违反就 500
        assert_eq!(
            LEGAL_EDGES.as_slice(),
            state_machine::Entity::CostObject.edges(),
            "biz_cost_object 的边表与状态机注册表不一致"
        );
        assert_eq!(LEGAL_EDGES.len(), 2, "只有关批 / 作废两条边");
        assert!(
            !LEGAL_EDGES
                .iter()
                .any(|(from, _)| *from == cost_object_dto::STATUS_CLOSED),
            "CLOSED 是终态：不可回 OPEN（设计 §4.1）"
        );
    }

    #[test]
    fn scope_target_points_at_workshop_column() {
        assert_eq!(OBJECT_TARGET.table, "biz_cost_object");
        assert_eq!(OBJECT_TARGET.alias, "t");
        assert_eq!(OBJECT_TARGET.dept_col, Some("workshop_dept_id"));
        assert!(OBJECT_TARGET.is_scoped(), "成本对象必须参与数据权限（设计 §5.4）");
        // 登记表里必须有它，否则 paginate_unscoped 会被误用
        assert!(
            scope::SCOPED_TABLES.iter().any(|item| item.table == "biz_cost_object"),
            "biz_cost_object 未登记进 SCOPED_TABLES"
        );
    }

    #[test]
    fn keyword_is_escaped_into_like_pattern() {
        assert_eq!(
            utils_like(&Some("  CB-2026  ".to_string())).as_deref(),
            Some("%CB-2026%")
        );
        assert_eq!(utils_like(&Some("a%b".to_string())).as_deref(), Some("%a\\%b%"));
        assert_eq!(utils_like(&Some("   ".to_string())), None);
        assert_eq!(utils_like(&None), None);
    }

    #[test]
    fn select_sql_joins_every_display_name() {
        let sql = select_sql();
        for needle in [
            "biz_material",
            "sys_dept",
            "biz_warehouse",
            "biz_location",
            "biz_customer",
            "product_name",
            "cost_total",
        ] {
            assert!(
                sql.contains(needle),
                "公共 SELECT 缺 {needle}（约定 13：*_id 必须带 *_name）"
            );
        }
    }

    #[test]
    fn order_by_has_full_order() {
        // 全序：状态 → 开工日 → 对象号 → id（翻页不漏行）
        assert!(OBJECT_ORDER.contains("t.id ASC"));
        assert!(OBJECT_ORDER.contains("t.object_no"));
    }

    #[test]
    fn qty_parser_reports_bad_input() {
        assert_eq!(parse_qty(" 12.5 ").expect("数量"), Decimal::from_str("12.5").unwrap());
        assert!(parse_qty("abc").is_err());
    }
}

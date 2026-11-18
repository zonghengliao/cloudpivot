//! 按生产订单/批次归集表 · 数据层（成本核算 · M5-02）。
//!
//! 表结构唯一依据：`biz_cost_object`（V0370 §3.1 ②）+ `biz_cost_collect`（V0371 §3.1 ③）。
//!
//! # 为什么这里不直接用 `scope::paginate`
//!
//! 架构 §2.6 的契约是：数据范围片段（`scope::filter_for` 产出的 ` AND t.… = ?`）
//! 的参数**恒前置**于业务参数，因此片段在 SQL 文本里必须出现在所有业务 `?` **之前**。
//!
//! 本表的料 / 工 / 费三列必须先在一张按批次聚合的派生表里算出（见 [`agg_sql`]），
//! 而那张派生表在 `FROM` 里 —— **文本位置在 `WHERE` 之前**。它的 `?`（期间）就必然
//! 排在片段参数前面。若强行交给 `scope::paginate`，参数列表与 SQL 里的 `?` 会错位，
//! 绑错值不报错、只算错账（比报错危险得多）。
//!
//! 所以本文件：
//!
//! 1. 数据范围**一律**用 `scope::filter_for(principal, SHEET_TARGET)` 取
//!    （不手写范围谓词 —— 登记表改一处就够，`SCOPED_TABLES` 锚点也在这里可查）；
//! 2. 参数顺序按 **SQL 文本顺序**显式构造，并用 [`bind`] 逐次校验
//!    「`?` 个数 == 参数个数」（错位立刻 500，绝不放过去绑错值）；
//! 3. count 与数据页共用同一份 `FROM … WHERE`，count 不带 `ORDER BY` / `LIMIT`。
//!
//! # 期间参数只出现一处（口径唯一）
//!
//! 料 / 工 / 费三项都在 [`agg_sql`] 里按 `period_code = ?` 聚合，**不在外层再筛一遍**：
//! 两处筛选会让人以为可以「外层全期间 + 内层本期」，那是最经典的对不上账的来源。
//!
//! # 名字怎么补（AGENTS 约定 13）
//!
//! | 列 | 口径 | 做法 |
//! |---|---|---|
//! | `product_id` → 编码 / 名称 | 主数据关联 ⇒ 实时取名 | `LEFT JOIN biz_material` |
//! | `workshop_dept_id` → 车间名 | 主数据关联 ⇒ 实时取名 | `LEFT JOIN sys_dept`，落空回落到建单快照 `t.workshop_dept_name` |
//! | `biz_cost_collect.source_no` / `dept_name` | 审计快照 | 原样读，不 join |
//!
//! 全部一次 join 拿全，行里没有逐行补名的 N+1。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_collect_sheet_dto::{self as dto, SheetFilter};
use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
use cloudpivot_kernel::business::shared::scope;

/// 数据权限锚点：**复用**登记表常量（`shared/scope.rs` 的 `SCOPED_TABLES` 那条）。
///
/// 别 `ScopeTarget::new(...)` 抄一份：抄本会让登记表与代码各自演进，而且
/// `permission::scope_model::tests::d8c_wiring_anchors_exist_in_source` 是按
/// 「登记表锚点常量名 + 同文件 scope 调用」校验接线的，抄本扫不到。
/// 与 `cost_object_model::OBJECT_TARGET` / `hr::employee_model::EMPLOYEE_TARGET` 同款。
pub const SHEET_TARGET: scope::ScopeTarget = scope::ScopeTarget::BIZ_COST_OBJECT;

/// 归集表一行的原始形态（金额列已按三要素拆好）。
#[derive(Debug, Clone)]
pub struct SheetRow {
    /// 成本对象 id。
    pub object_id: Uuid,
    /// 批次号。
    pub object_no: String,
    /// 对象类型。
    pub object_type: String,
    /// 生产订单号。
    pub source_doc_no: Option<String>,
    /// 产出批号。
    pub lot_no: Option<String>,
    /// 产出品 id。
    pub product_id: Uuid,
    /// 产出品编码。
    pub product_code: Option<String>,
    /// 产出品名称。
    pub product_name: Option<String>,
    /// 生产车间 id。
    pub workshop_dept_id: Uuid,
    /// 生产车间名称。
    pub workshop_dept_name: Option<String>,
    /// 计划数量。
    pub plan_qty: Decimal,
    /// 累计完工数量。
    pub completed_qty: Decimal,
    /// 期末在制数量。
    pub wip_qty: Decimal,
    /// 完工进度%。
    pub completion_pct: Decimal,
    /// 状态。
    pub status: String,
    /// 开工日期。
    pub start_date: NaiveDate,
    /// 计划完工日期。
    pub due_date: Option<NaiveDate>,
    /// 本期直接材料。
    pub mat_amount: Decimal,
    /// 本期直接人工。
    pub labor_amount: Decimal,
    /// 本期制造费用。
    pub oh_amount: Decimal,
    /// 本期归集行数。
    pub line_count: i64,
    /// 本期最后一笔归集的业务日期。
    pub last_occur_date: Option<NaiveDate>,
}

impl SheetRow {
    /// 行 → 出参。
    pub fn to_res(&self) -> dto::SheetRowRes {
        dto::SheetRowRes {
            object_id: self.object_id.to_string(),
            object_no: self.object_no.clone(),
            object_type: self.object_type.clone(),
            source_doc_no: self.source_doc_no.clone(),
            lot_no: self.lot_no.clone(),
            product_id: self.product_id.to_string(),
            product_code: self.product_code.clone(),
            product_name: self.product_name.clone(),
            workshop_dept_id: self.workshop_dept_id.to_string(),
            workshop_dept_name: self.workshop_dept_name.clone(),
            plan_qty: dto::amount_text(self.plan_qty),
            completed_qty: dto::amount_text(self.completed_qty),
            wip_qty: dto::amount_text(self.wip_qty),
            completion_pct: dto::amount_text(self.completion_pct),
            status: self.status.clone(),
            start_date: self.start_date.to_string(),
            due_date: dto::date_text(self.due_date),
            amounts: dto::SheetAmounts::of(self.mat_amount, self.labor_amount, self.oh_amount),
            line_count: self.line_count,
            last_occur_date: dto::date_text(self.last_occur_date),
        }
    }
}

/// 整张表的合计（不是当前页）。
#[derive(Debug, Clone, Copy, Default)]
pub struct SheetTotals {
    /// 直接材料。
    pub mat: Decimal,
    /// 直接人工。
    pub labor: Decimal,
    /// 制造费用。
    pub oh: Decimal,
    /// 归集行数。
    pub line_count: i64,
    /// 有归集的批次数。
    pub batch_count: i64,
}

/// 明细接口的批次抬头。
#[derive(Debug, Clone)]
pub struct BatchRow {
    /// 成本对象 id。
    pub object_id: Uuid,
    /// 批次号。
    pub object_no: String,
    /// 对象类型。
    pub object_type: String,
    /// 生产订单号。
    pub source_doc_no: Option<String>,
    /// 产出批号。
    pub lot_no: Option<String>,
    /// 产出品 id。
    pub product_id: Uuid,
    /// 产出品编码。
    pub product_code: Option<String>,
    /// 产出品名称。
    pub product_name: Option<String>,
    /// 生产车间 id。
    pub workshop_dept_id: Uuid,
    /// 生产车间名称。
    pub workshop_dept_name: Option<String>,
    /// 计划数量。
    pub plan_qty: Decimal,
    /// 累计完工数量。
    pub completed_qty: Decimal,
    /// 期末在制数量。
    pub wip_qty: Decimal,
    /// 完工进度%。
    pub completion_pct: Decimal,
    /// 状态。
    pub status: String,
    /// 开工日期。
    pub start_date: NaiveDate,
    /// 计划完工日期。
    pub due_date: Option<NaiveDate>,
}

impl BatchRow {
    /// 抬头 → 出参。
    pub fn to_res(&self) -> dto::SheetBatchRes {
        dto::SheetBatchRes {
            object_id: self.object_id.to_string(),
            object_no: self.object_no.clone(),
            object_type: self.object_type.clone(),
            source_doc_no: self.source_doc_no.clone(),
            lot_no: self.lot_no.clone(),
            product_id: self.product_id.to_string(),
            product_code: self.product_code.clone(),
            product_name: self.product_name.clone(),
            workshop_dept_id: self.workshop_dept_id.to_string(),
            workshop_dept_name: self.workshop_dept_name.clone(),
            plan_qty: dto::amount_text(self.plan_qty),
            completed_qty: dto::amount_text(self.completed_qty),
            wip_qty: dto::amount_text(self.wip_qty),
            completion_pct: dto::amount_text(self.completion_pct),
            status: self.status.clone(),
            start_date: self.start_date.to_string(),
            due_date: dto::date_text(self.due_date),
        }
    }
}

/// 一笔归集行。
#[derive(Debug, Clone)]
pub struct LineRow {
    /// 归集行 id。
    pub id: Uuid,
    /// 要素。
    pub element: String,
    /// 来源类型。
    pub source_type: String,
    /// 来源行 id。
    pub source_id: Option<Uuid>,
    /// 来源单号。
    pub source_no: Option<String>,
    /// 来源行号。
    pub line_no: i32,
    /// 业务日期。
    pub occur_date: NaiveDate,
    /// 物料 id。
    pub material_id: Option<Uuid>,
    /// 物料编码。
    pub material_code: Option<String>,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 数量。
    pub qty: Option<Decimal>,
    /// 单价。
    pub unit_price: Option<Decimal>,
    /// 金额。
    pub amount: Decimal,
    /// 车间名快照。
    pub dept_name: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

impl LineRow {
    /// 行 → 出参。
    pub fn to_res(&self) -> dto::SheetLineRes {
        dto::SheetLineRes {
            id: self.id.to_string(),
            element: self.element.clone(),
            element_label: dto::element_label(&self.element).to_string(),
            source_type: self.source_type.clone(),
            source_type_label: dto::source_type_label(&self.source_type).to_string(),
            source_id: self.source_id.map(|id| id.to_string()),
            source_no: self.source_no.clone(),
            line_no: self.line_no,
            occur_date: self.occur_date.to_string(),
            material_id: self.material_id.map(|id| id.to_string()),
            material_code: self.material_code.clone(),
            material_name: self.material_name.clone(),
            qty: self.qty.map(dto::amount_text),
            unit_price: self.unit_price.map(dto::amount_text),
            amount: dto::amount_text(self.amount),
            dept_name: self.dept_name.clone(),
            remark: self.remark.clone(),
        }
    }
}

// ==================== SQL 拼装 ====================

/// 按批次聚合本期料 / 工 / 费的派生表（**唯一**出现期间参数 `?` 的地方）。
///
/// `FILTER (WHERE element = …)` 而不是三个 `CASE WHEN` 子查询：一次扫描算完三项
/// （走 `idx_biz_cost_collect_obj` 的 `(cost_object_id, period_code, element)` 前缀），
/// 而且三列互斥、不会重复计数。
///
/// `count(*)` 在派生表里数的是**本期该批的归集行数**（不是 `count(amount)`：
/// `amount` 是 `NOT NULL`，两者同值，但 `count(*)` 更直白）。
fn agg_sql() -> &'static str {
    "LEFT JOIN ( \
         SELECT c.cost_object_id, \
                round(COALESCE(sum(c.amount) FILTER (WHERE c.element = 'MATERIAL'), 0), 4) AS mat_amount, \
                round(COALESCE(sum(c.amount) FILTER (WHERE c.element = 'LABOR'), 0), 4) AS labor_amount, \
                round(COALESCE(sum(c.amount) FILTER (WHERE c.element = 'OVERHEAD'), 0), 4) AS oh_amount, \
                count(*) AS line_count, \
                max(c.occur_date) AS last_occur_date \
           FROM biz_cost_collect c \
          WHERE c.deleted_at IS NULL AND c.cost_object_id IS NOT NULL AND c.period_code = ? \
          GROUP BY c.cost_object_id \
     ) a ON a.cost_object_id = t.id"
}

/// 归集表的公共 `SELECT … FROM …`（参数：`agg_sql` 里那个期间）。
fn list_sql() -> String {
    format!(
        "SELECT t.id AS object_id, t.object_no, t.object_type, t.source_doc_no, t.lot_no, \
                t.product_id, m.material_code AS product_code, m.material_name AS product_name, \
                t.workshop_dept_id, COALESCE(d.dept_name, t.workshop_dept_name) AS workshop_dept_name, \
                t.plan_qty, t.completed_qty, t.wip_qty, t.completion_pct, t.status, \
                t.start_date, t.due_date, \
                round(COALESCE(a.mat_amount, 0), 4) AS mat_amount, \
                round(COALESCE(a.labor_amount, 0), 4) AS labor_amount, \
                round(COALESCE(a.oh_amount, 0), 4) AS oh_amount, \
                COALESCE(a.line_count, 0) AS line_count, \
                a.last_occur_date \
           FROM biz_cost_object t \
           LEFT JOIN biz_material m ON m.id = t.product_id \
           LEFT JOIN sys_dept d ON d.id = t.workshop_dept_id AND d.deleted_at IS NULL \
           {}",
        agg_sql()
    )
}

/// 明细接口的批次抬头 `SELECT … FROM …`（无参数；期间不进抬头）。
fn batch_sql() -> &'static str {
    "SELECT t.id AS object_id, t.object_no, t.object_type, t.source_doc_no, t.lot_no, \
            t.product_id, m.material_code AS product_code, m.material_name AS product_name, \
            t.workshop_dept_id, COALESCE(d.dept_name, t.workshop_dept_name) AS workshop_dept_name, \
            t.plan_qty, t.completed_qty, t.wip_qty, t.completion_pct, t.status, \
            t.start_date, t.due_date \
       FROM biz_cost_object t \
       LEFT JOIN biz_material m ON m.id = t.product_id \
       LEFT JOIN sys_dept d ON d.id = t.workshop_dept_id AND d.deleted_at IS NULL"
}

/// 业务过滤条件（**不含**数据范围片段、**不含**期间）。
///
/// 参数的排列与 `?` 在返回 SQL 里的出现顺序**逐字对应**（列表 SQL 里这些条件
/// 一定拼在数据范围片段之后，见 [`page_rows`] 的参数顺序说明）。
fn business_where(filter: &SheetFilter) -> (String, Vec<Value>) {
    let mut sql = String::new();
    let mut params: Vec<Value> = Vec::new();
    if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND t.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(dept) = filter.workshop_dept_id {
        sql.push_str(" AND t.workshop_dept_id = ?::uuid");
        params.push(Value::from(&dept));
    }
    if let Some(pattern) = keyword_pattern(filter.keyword.as_deref()) {
        // 批次号 / 生产订单号 / 产出批号 / 产出品编码 / 产出品名称：
        // 用户手上可能只有其中任一个（订单号来自销售、批号来自仓库、编码来自物料卡）。
        sql.push_str(
            " AND (t.object_no LIKE ? OR COALESCE(t.source_doc_no, '') LIKE ? \
                OR COALESCE(t.lot_no, '') LIKE ? OR COALESCE(m.material_code, '') LIKE ? \
                OR COALESCE(m.material_name, '') LIKE ?)",
        );
        for _ in 0..5 {
            params.push(Value::Text(pattern.clone()));
        }
    }
    (sql, params)
}

/// 关键字 → LIKE 模式（`%` / `_` 转义，与 `utils::like_pattern` 同口径 + 转义）。
fn keyword_pattern(keyword: Option<&str>) -> Option<String> {
    keyword.map(str::trim).filter(|text| !text.is_empty()).map(|text| {
        format!(
            "%{}%",
            text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
        )
    })
}

/// 占位符 / 参数一致性校验（**有牙**：错位立刻 500，绝不绑错值算错账）。
///
/// `?` 的个数必须等于参数个数。这条不是形式主义：本文件是手工拼参数顺序的
/// （见模块头注释），少推一个 `Value`、或把片段参数放到期间参数前面，
/// 驱动不会报错，只会把 `'2026-11'` 绑到 `t.status` 上然后返回一个空表。
fn bind(sql: &str, params: &[Value]) -> DbResult<()> {
    let placeholders = sql.matches('?').count();
    if placeholders != params.len() {
        return Err(DbError::InvalidValue(format!(
            "按批次归集表 SQL 占位符与参数数量不一致（?={placeholders}，参数={}）：参数错位会静默绑错值，\
             拒绝执行。SQL={sql}",
            params.len()
        )));
    }
    Ok(())
}

// ==================== 读：归集表 ====================

/// 归集表分页（数据范围 = 成本对象列表的同一份）。
///
/// # 参数顺序（文本顺序，**改 SQL 必须同步改这里**）
///
/// ```text
/// ① 期间        agg_sql 的 c.period_code = ?     ← FROM 里的派生表，文本最前
/// ② 数据范围    scope_filter.sql() 的 ?          ← 第一层 WHERE
/// ③ 业务条件    status / 车间 / 关键字           ← 第一层 WHERE（片段之后）
/// ④ 分页        LIMIT ? OFFSET ?                 ← 只在数据页，count 没有
/// ```
pub async fn page_rows(principal: &ScopedPrincipal, filter: &SheetFilter) -> DbResult<(Vec<SheetRow>, i64)> {
    let scope_filter = scope::filter_for(principal, SHEET_TARGET);
    let (biz_sql, biz_params) = business_where(filter);
    let base = format!(
        "{} WHERE t.deleted_at IS NULL {}{}",
        list_sql(),
        scope_filter.sql(),
        biz_sql
    );

    let mut params: Vec<Value> = Vec::with_capacity(2 + scope_filter.params().len() + biz_params.len());
    params.push(Value::Text(filter.period_code.clone()));
    params.extend_from_slice(scope_filter.params());
    params.extend(biz_params);

    // ---- count：同一份 FROM/WHERE，不带 ORDER BY / LIMIT ----
    let count_sql = format!("SELECT count(*) AS total FROM ({base}) AS q");
    bind(&count_sql, &params)?;
    let count_row = db().query_first_sql(count_sql, params.clone()).await?;
    let total: i64 = match count_row.as_ref() {
        Some(row) => row.get_typed("total")?,
        None => 0,
    };

    // ---- 数据页 ----
    let offset = filter.page.saturating_sub(1).saturating_mul(filter.page_size);
    let mut page_params = params;
    page_params.push(Value::I64(i64::try_from(filter.page_size).unwrap_or(i64::MAX)));
    page_params.push(Value::I64(i64::try_from(offset).unwrap_or(i64::MAX)));
    let sql = format!("{base} {} LIMIT ? OFFSET ?", dto::order_by_sql(&filter.sort));
    bind(&sql, &page_params)?;
    let rows = db().query_sql(sql, page_params).await?;

    let list = rows.iter().map(sheet_row_of).collect::<DbResult<Vec<SheetRow>>>()?;
    Ok((list, total))
}

/// 由查询行构造一条归集表行。
fn sheet_row_of(row: &Row) -> DbResult<SheetRow> {
    let snapshot: Option<String> = row.get_typed("workshop_dept_name")?;
    Ok(SheetRow {
        object_id: row.get_typed("object_id")?,
        object_no: row.get_typed("object_no")?,
        object_type: row.get_typed("object_type")?,
        source_doc_no: row.get_typed("source_doc_no")?,
        lot_no: row.get_typed("lot_no")?,
        product_id: row.get_typed("product_id")?,
        product_code: row.get_typed("product_code")?,
        product_name: row.get_typed("product_name")?,
        workshop_dept_id: row.get_typed("workshop_dept_id")?,
        workshop_dept_name: snapshot.filter(|text| !text.is_empty()),
        plan_qty: row.get_typed("plan_qty")?,
        completed_qty: row.get_typed("completed_qty")?,
        wip_qty: row.get_typed("wip_qty")?,
        completion_pct: row.get_typed("completion_pct")?,
        status: row.get_typed("status")?,
        start_date: row.get_typed("start_date")?,
        due_date: row.get_typed("due_date")?,
        mat_amount: row.get_typed("mat_amount")?,
        labor_amount: row.get_typed("labor_amount")?,
        oh_amount: row.get_typed("oh_amount")?,
        line_count: row.get_typed("line_count")?,
        last_occur_date: row.get_typed("last_occur_date")?,
    })
}

/// 整张表的合计（与 [`page_rows`] 同一份筛选，**不受分页影响**）。
///
/// 参数顺序：① 数据范围（片段文本最前）② 期间 ③ 业务条件
/// —— 与 [`page_rows`] 不同，因为这里没有派生表，片段写在 `WHERE` 的第一位。
pub async fn totals(principal: &ScopedPrincipal, filter: &SheetFilter) -> DbResult<SheetTotals> {
    let scope_filter = scope::filter_for(principal, SHEET_TARGET);
    let (biz_sql, biz_params) = business_where(filter);
    let sql = format!(
        "SELECT round(COALESCE(sum(c.amount) FILTER (WHERE c.element = 'MATERIAL'), 0), 4) AS mat_amount, \
                round(COALESCE(sum(c.amount) FILTER (WHERE c.element = 'LABOR'), 0), 4) AS labor_amount, \
                round(COALESCE(sum(c.amount) FILTER (WHERE c.element = 'OVERHEAD'), 0), 4) AS oh_amount, \
                count(c.id) AS line_count, \
                count(DISTINCT c.cost_object_id) AS batch_count \
           FROM biz_cost_collect c \
           JOIN biz_cost_object t ON t.id = c.cost_object_id \
           LEFT JOIN biz_material m ON m.id = t.product_id \
          WHERE t.deleted_at IS NULL AND c.deleted_at IS NULL {} AND c.period_code = ?{}",
        scope_filter.sql(),
        biz_sql
    );

    let mut params: Vec<Value> = Vec::with_capacity(1 + scope_filter.params().len() + biz_params.len());
    params.extend_from_slice(scope_filter.params());
    params.push(Value::Text(filter.period_code.clone()));
    params.extend(biz_params);

    bind(&sql, &params)?;
    let row = db()
        .query_first_sql(sql, params)
        .await?
        .ok_or_else(|| DbError::InvalidValue("按批次归集表合计查询没有返回行（聚合查询必有一行）".to_string()))?;
    Ok(SheetTotals {
        mat: row.get_typed("mat_amount")?,
        labor: row.get_typed("labor_amount")?,
        oh: row.get_typed("oh_amount")?,
        line_count: row.get_typed("line_count")?,
        batch_count: row.get_typed("batch_count")?,
    })
}

// ==================== 读：明细 ====================

/// 按主键取**在当前用户数据范围内**的批次抬头；不可见 / 不存在 / 已删 ⇒ `None`。
///
/// 单条入口与列表同一份探针（AGENTS 约定：不可见报 404，不报 403 —— 403 会告诉
/// 调用方「这个 id 真实存在」，等于把别的车间的批次号当探测口）。
pub async fn find_visible_batch(principal: &ScopedPrincipal, id: Uuid) -> DbResult<Option<BatchRow>> {
    let scope_filter = scope::filter_for(principal, SHEET_TARGET);
    let sql = format!(
        "{} WHERE t.id = ?::uuid AND t.deleted_at IS NULL {}",
        batch_sql(),
        scope_filter.sql()
    );
    let mut params: Vec<Value> = Vec::with_capacity(1 + scope_filter.params().len());
    params.push(Value::from(&id));
    params.extend_from_slice(scope_filter.params());

    bind(&sql, &params)?;
    let rows = db().query_sql(sql, params).await?;
    rows.first().map(batch_row_of).transpose()
}

/// 由查询行构造批次抬头。
fn batch_row_of(row: &Row) -> DbResult<BatchRow> {
    let snapshot: Option<String> = row.get_typed("workshop_dept_name")?;
    Ok(BatchRow {
        object_id: row.get_typed("object_id")?,
        object_no: row.get_typed("object_no")?,
        object_type: row.get_typed("object_type")?,
        source_doc_no: row.get_typed("source_doc_no")?,
        lot_no: row.get_typed("lot_no")?,
        product_id: row.get_typed("product_id")?,
        product_code: row.get_typed("product_code")?,
        product_name: row.get_typed("product_name")?,
        workshop_dept_id: row.get_typed("workshop_dept_id")?,
        workshop_dept_name: snapshot.filter(|text| !text.is_empty()),
        plan_qty: row.get_typed("plan_qty")?,
        completed_qty: row.get_typed("completed_qty")?,
        wip_qty: row.get_typed("wip_qty")?,
        completion_pct: row.get_typed("completion_pct")?,
        status: row.get_typed("status")?,
        start_date: row.get_typed("start_date")?,
        due_date: row.get_typed("due_date")?,
    })
}

/// 某一批次在某一期间的全部归集行（料 → 工 → 费，再按业务日期）。
///
/// 调用前必须已用 [`find_visible_batch`] 做过数据范围探针：本函数**不再**注入范围
/// （`biz_cost_collect` 没有车间列语义上的成本对象归属：池行的 `dept_id` 是费用
/// 发生车间，不是批次的车间；再筛一遍会与探针口径打架）。这一点写在这里，
/// 是为了让下一个改本文件的人不敢「顺手加个 scope」。
pub async fn lines_of(object_id: Uuid, period_code: &str) -> DbResult<Vec<LineRow>> {
    let sql = "SELECT c.id, c.element, c.source_type, c.source_id, c.source_no, c.line_no, \
                      c.occur_date, c.material_id, mm.material_code AS material_code, \
                      mm.material_name AS material_name, c.qty, c.unit_price, c.amount, \
                      c.dept_name, c.remark \
                 FROM biz_cost_collect c \
                 LEFT JOIN biz_material mm ON mm.id = c.material_id \
                WHERE c.cost_object_id = ?::uuid AND c.period_code = ? AND c.deleted_at IS NULL \
                ORDER BY CASE c.element WHEN 'MATERIAL' THEN 1 WHEN 'LABOR' THEN 2 ELSE 3 END, \
                         c.occur_date ASC, c.source_no ASC NULLS LAST, c.line_no ASC, c.id ASC"
        .to_string();
    let params = vec![Value::from(&object_id), Value::Text(period_code.to_string())];
    bind(&sql, &params)?;
    let rows = db().query_sql(sql, params).await?;
    rows.iter().map(line_row_of).collect()
}

/// 由查询行构造一笔归集行。
fn line_row_of(row: &Row) -> DbResult<LineRow> {
    Ok(LineRow {
        id: row.get_typed("id")?,
        element: row.get_typed("element")?,
        source_type: row.get_typed("source_type")?,
        source_id: row.get_typed("source_id")?,
        source_no: row.get_typed("source_no")?,
        line_no: row.get_typed("line_no")?,
        occur_date: row.get_typed("occur_date")?,
        material_id: row.get_typed("material_id")?,
        material_code: row.get_typed("material_code")?,
        material_name: row.get_typed("material_name")?,
        qty: row.get_typed("qty")?,
        unit_price: row.get_typed("unit_price")?,
        amount: row.get_typed("amount")?,
        dept_name: row.get_typed("dept_name")?,
        remark: row.get_typed("remark")?,
    })
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(status: Option<&str>, dept: Option<Uuid>, keyword: Option<&str>) -> SheetFilter {
        SheetFilter {
            period_code: "2026-11".to_string(),
            keyword: keyword.map(str::to_string),
            workshop_dept_id: dept,
            status: status.map(str::to_string),
            sort: dto::SORT_TOTAL.to_string(),
            page: 1,
            page_size: 20,
        }
    }

    #[test]
    fn business_where_placeholders_match_params() {
        // 无筛选：0 个 `?`
        let (sql, params) = business_where(&filter(None, None, None));
        assert_eq!(sql.matches('?').count(), 0);
        assert!(params.is_empty());

        // 状态 + 车间 + 关键字（5 个 LIKE）：1 + 1 + 5 = 7 个 `?`
        let (sql, params) = business_where(&filter(Some("OPEN"), Some(Uuid::nil()), Some("SO-1")));
        assert_eq!(sql.matches('?').count(), 7, "SQL={sql}");
        assert_eq!(params.len(), 7);
        // 车间参数必须绑成 uuid 形态（`?::uuid`），否则 PG 会按文本比 uuid 列
        assert!(sql.contains("t.workshop_dept_id = ?::uuid"));
    }

    #[test]
    fn keyword_is_escaped() {
        // `%` / `_` 必须转义：否则用户搜 `A_B` 会匹配 `AXB`（模糊成通配）
        assert_eq!(keyword_pattern(Some(" A_B ")).as_deref(), Some("%A\\_B%"));
        assert_eq!(keyword_pattern(Some("100%")).as_deref(), Some("%100\\%%"));
        assert_eq!(keyword_pattern(Some("   ")), None, "纯空白 = 不筛");
        assert_eq!(keyword_pattern(None), None);
    }

    #[test]
    fn agg_sql_has_exactly_one_period_placeholder() {
        // 期间只允许出现一处（模块头「口径唯一」）：多一处就会有两种归期口径
        assert_eq!(agg_sql().matches('?').count(), 1);
        // 池行（cost_object_id 为空）不进批表：它们是待分摊的车间费用
        assert!(agg_sql().contains("c.cost_object_id IS NOT NULL"));
        // 三要素都用 FILTER 拆，不用三条子查询
        assert_eq!(agg_sql().matches("FILTER (WHERE c.element").count(), 3);
    }

    #[test]
    fn bind_guard_has_teeth() {
        let sql = "SELECT 1 FROM t WHERE a = ? AND b = ?";
        assert!(bind(sql, &[Value::Text("x".into()), Value::Text("y".into())]).is_ok());
        // 少一个参数 ⇒ 必须报错（错位会静默绑错值）
        let error = bind(sql, &[Value::Text("x".into())]).expect_err("少参数必须拒绝执行");
        assert!(format!("{error}").contains("占位符"), "{error}");
        // 多一个参数 ⇒ 同样拒绝
        let error = bind(
            sql,
            &[
                Value::Text("x".into()),
                Value::Text("y".into()),
                Value::Text("z".into()),
            ],
        )
        .expect_err("多参数必须拒绝执行");
        assert!(format!("{error}").contains("占位符"), "{error}");
    }

    #[test]
    fn list_sql_joins_names_in_one_pass() {
        let sql = list_sql();
        // 产出品 / 车间名各一次 join（没有逐行补名的 N+1）
        assert_eq!(sql.matches("LEFT JOIN biz_material").count(), 1);
        assert_eq!(sql.matches("LEFT JOIN sys_dept").count(), 1);
        // 车间名实时优先、快照兜底
        assert!(sql.contains("COALESCE(d.dept_name, t.workshop_dept_name)"));
        // 期间 `?` 只有一个（在派生表里）
        assert_eq!(sql.matches('?').count(), 1);
    }
}

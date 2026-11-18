//! 分批法「在制批次余额」· 数据层（成本核算 · M5-05）。
//!
//! 表结构唯一依据：`biz_cost_object`（V0370）+ `biz_cost_collect`（V0371）+
//! `biz_cost_batch_finish`（V0461）+ `biz_cost_period_item` / `biz_cost_period`（V0373）。
//!
//! # 口径（与 V0461 表注释逐字一致）
//!
//! ```text
//! 累计归集(P) = Σ biz_cost_collect.amount        WHERE deleted_at IS NULL AND period_code ≤ P
//! 累计结转(P) = Σ biz_cost_batch_finish.*_amount WHERE deleted_at IS NULL AND period_code ≤ P
//! 在制余额(P) = 累计归集(P) − 累计结转(P)        （逐要素，不夹零）
//! ```
//!
//! **期间是「≤ P」而不是「= P」**：在制余额是时点量，P 之后的结转（下一期才做的）
//! 不能冲减 P 期末的余额；P 本身的归集才算进来。两个边界都只用 `period_code <= ?`
//! 一处，绝不外层再筛一遍（两处筛选是「对不上账」的经典来源）。
//!
//! # 为什么参数顺序是手工排的（不用 `scope::paginate`）
//!
//! 与 M5-02 归集表同因：料 / 工 / 费与已结转金额都在 `FROM` 的派生表里先聚合，
//! 派生表的 `?`（四个期间参数）在 SQL 文本里**排在数据范围片段之前**；交给
//! `scope::paginate` 会让 `?` 与参数列表错位 —— 绑错值不报错、只算错账。
//! 所以参数按**文本顺序**显式构造，并用 [`bind`] 逐次校验「`?` 个数 == 参数个数」。
//!
//! # 名字怎么补（AGENTS 约定 13）
//!
//! | 列 | 口径 | 做法 |
//! |---|---|---|
//! | `product_id` → 编码 / 名称 | 主数据关联 ⇒ 实时取名 | `LEFT JOIN biz_material` |
//! | `workshop_dept_id` → 车间名 | 主数据关联 ⇒ 实时取名，落空回落到建单快照 | `LEFT JOIN sys_dept` |
//!
//! 一次 join 拿全，行里没有逐行补名的 N+1。

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_batch_finish_dto::Amounts;
use crate::business::erp::cost::cost_batch_wip_dto::{self as dto, PeriodFlow, SettledFacts, WipFacts, WipFilter};
use crate::business::erp::cost::cost_collect_sheet_dto;
use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
use cloudpivot_kernel::business::shared::scope;

/// 数据权限锚点：**复用**登记表常量（`shared/scope.rs` 的 `SCOPED_TABLES` 那条）。
///
/// 别 `ScopeTarget::new(...)` 抄一份：抄本会让登记表与代码各自演进，而且
/// `permission::scope_model::tests::d8c_wiring_anchors_exist_in_source` 是按
/// 「登记表锚点常量名 + 同文件 scope 调用」校验接线的，抄本扫不到。
pub const WIP_TARGET: scope::ScopeTarget = scope::ScopeTarget::BIZ_COST_OBJECT;

// ==================== 行 ====================

/// 在制余额表一行的原始形态。
#[derive(Debug, Clone)]
pub struct WipRow {
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
    /// 批次状态。
    pub status: String,
    /// 开工日期。
    pub start_date: nik::prelude::chrono::NaiveDate,
    /// 计划完工日期。
    pub due_date: Option<nik::prelude::chrono::NaiveDate>,
    /// 累计归集。
    pub collected: Amounts,
    /// 累计已结转。
    pub transferred: Amounts,
    /// 本期投入。
    pub period_in: Amounts,
    /// 本期转出。
    pub period_out: Amounts,
    /// 累计归集行数。
    pub collect_lines: i64,
    /// 累计结转次数。
    pub transfer_times: i64,
    /// 首次归集期间。
    pub first_period: Option<String>,
    /// 最后一次归集期间。
    pub last_period: Option<String>,
    /// 最近一次结转时间。
    pub last_transfer_at: Option<DateTime<Utc>>,
}

impl WipRow {
    /// 在制余额（逐要素相减，不夹零）。
    pub fn balance(&self) -> Amounts {
        dto::balance_of(&self.collected, &self.transferred)
    }

    /// 行 → 出参。
    pub fn to_res(&self) -> dto::WipRowRes {
        let balance = self.balance();
        let facts = WipFacts {
            status: &self.status,
            completed_qty: self.completed_qty,
            wip_qty: self.wip_qty,
            balance,
        };
        dto::WipRowRes {
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
            plan_qty: cost_collect_sheet_dto::amount_text(self.plan_qty),
            completed_qty: cost_collect_sheet_dto::amount_text(self.completed_qty),
            wip_qty: cost_collect_sheet_dto::amount_text(self.wip_qty),
            completion_pct: cost_collect_sheet_dto::amount_text(self.completion_pct),
            status: self.status.clone(),
            start_date: self.start_date.to_string(),
            due_date: self.due_date.map(|date| date.to_string()),
            collected: dto::WipAmountsRes::of(&self.collected),
            transferred: dto::WipAmountsRes::of(&self.transferred),
            balance: dto::WipAmountsRes::of(&balance),
            period_in: dto::WipAmountsRes::of(&self.period_in),
            period_out: dto::WipAmountsRes::of(&self.period_out),
            collect_lines: self.collect_lines,
            transfer_times: self.transfer_times,
            first_period: self.first_period.clone(),
            last_period: self.last_period.clone(),
            last_transfer_at: self.last_transfer_at.map(|at| at.to_rfc3339()),
            has_balance: balance.total() > Decimal::ZERO,
            warnings: dto::evaluate_warnings(&facts)
                .into_iter()
                .map(|warning| warning.code)
                .collect(),
        }
    }
}

/// 整张表的合计（**不是当前页**）。
#[derive(Debug, Clone, Copy, Default)]
pub struct WipTotals {
    /// 累计归集合计。
    pub collected: Amounts,
    /// 累计已结转合计。
    pub transferred: Amounts,
    /// 本期投入合计。
    pub period_in: Amounts,
    /// 本期转出合计。
    pub period_out: Amounts,
    /// 批次数。
    pub batch_count: i64,
    /// 有余额的批次数。
    pub wip_batch_count: i64,
    /// 余额为负的批次数。
    pub negative_batch_count: i64,
}

// ==================== SQL 拼装 ====================

/// 三个派生表（累计归集 / 累计结转 / 本期投入 / 本期转出）——**唯一**出现期间参数的四处。
///
/// 四条子查询都走既有索引：
/// * 归集：`idx_biz_cost_collect_obj (cost_object_id, period_code, element)`；
/// * 结转：`uk_biz_cost_batch_finish_seq (object_id, transfer_seq)` 的前缀。
fn joins_sql() -> &'static str {
    concat!(
        // ① 累计归集（period_code ≤ 截止期间）
        "LEFT JOIN ( ",
        "  SELECT c.cost_object_id, ",
        "         sum(c.amount) FILTER (WHERE c.element = 'MATERIAL') AS mat_amount, ",
        "         sum(c.amount) FILTER (WHERE c.element = 'LABOR') AS labor_amount, ",
        "         sum(c.amount) FILTER (WHERE c.element = 'OVERHEAD') AS oh_amount, ",
        "         count(*) AS line_count, ",
        "         min(c.period_code) AS first_period, ",
        "         max(c.period_code) AS last_period ",
        "    FROM biz_cost_collect c ",
        "   WHERE c.deleted_at IS NULL AND c.cost_object_id IS NOT NULL AND c.period_code <= ? ",
        "   GROUP BY c.cost_object_id ",
        ") c ON c.cost_object_id = t.id ",
        // ② 累计结转（period_code ≤ 截止期间）
        "LEFT JOIN ( ",
        "  SELECT f.object_id, ",
        "         sum(f.material_amount) AS mat_amount, ",
        "         sum(f.labor_amount) AS labor_amount, ",
        "         sum(f.overhead_amount) AS oh_amount, ",
        "         count(*) AS times, ",
        "         max(f.created_at) AS last_at ",
        "    FROM biz_cost_batch_finish f ",
        "   WHERE f.deleted_at IS NULL AND f.period_code <= ? ",
        "   GROUP BY f.object_id ",
        ") f ON f.object_id = t.id ",
        // ③ 本期投入（period_code = 截止期间）
        "LEFT JOIN ( ",
        "  SELECT ci.cost_object_id, ",
        "         sum(ci.amount) FILTER (WHERE ci.element = 'MATERIAL') AS mat_amount, ",
        "         sum(ci.amount) FILTER (WHERE ci.element = 'LABOR') AS labor_amount, ",
        "         sum(ci.amount) FILTER (WHERE ci.element = 'OVERHEAD') AS oh_amount ",
        "    FROM biz_cost_collect ci ",
        "   WHERE ci.deleted_at IS NULL AND ci.cost_object_id IS NOT NULL AND ci.period_code = ? ",
        "   GROUP BY ci.cost_object_id ",
        ") ci ON ci.cost_object_id = t.id ",
        // ④ 本期转出（period_code = 截止期间）
        "LEFT JOIN ( ",
        "  SELECT fi.object_id, ",
        "         sum(fi.material_amount) AS mat_amount, ",
        "         sum(fi.labor_amount) AS labor_amount, ",
        "         sum(fi.overhead_amount) AS oh_amount ",
        "    FROM biz_cost_batch_finish fi ",
        "   WHERE fi.deleted_at IS NULL AND fi.period_code = ? ",
        "   GROUP BY fi.object_id ",
        ") fi ON fi.object_id = t.id"
    )
}

/// 公共 `SELECT … FROM … `（参数：四个期间，按文本顺序）。
fn list_sql() -> String {
    format!(
        concat!(
            "SELECT t.id AS object_id, t.object_no, t.object_type, t.source_doc_no, t.lot_no, ",
            "       t.product_id, m.material_code AS product_code, m.material_name AS product_name, ",
            "       t.workshop_dept_id, COALESCE(d.dept_name, t.workshop_dept_name) AS workshop_dept_name, ",
            "       t.plan_qty, t.completed_qty, t.wip_qty, t.completion_pct, t.status, ",
            "       t.start_date, t.due_date, ",
            "       round(COALESCE(c.mat_amount, 0), 4) AS col_mat, ",
            "       round(COALESCE(c.labor_amount, 0), 4) AS col_labor, ",
            "       round(COALESCE(c.oh_amount, 0), 4) AS col_oh, ",
            "       COALESCE(c.line_count, 0) AS col_lines, c.first_period, c.last_period, ",
            "       round(COALESCE(f.mat_amount, 0), 4) AS tr_mat, ",
            "       round(COALESCE(f.labor_amount, 0), 4) AS tr_labor, ",
            "       round(COALESCE(f.oh_amount, 0), 4) AS tr_oh, ",
            "       COALESCE(f.times, 0) AS tr_times, f.last_at, ",
            "       round(COALESCE(ci.mat_amount, 0), 4) AS in_mat, ",
            "       round(COALESCE(ci.labor_amount, 0), 4) AS in_labor, ",
            "       round(COALESCE(ci.oh_amount, 0), 4) AS in_oh, ",
            "       round(COALESCE(fi.mat_amount, 0), 4) AS out_mat, ",
            "       round(COALESCE(fi.labor_amount, 0), 4) AS out_labor, ",
            "       round(COALESCE(fi.oh_amount, 0), 4) AS out_oh ",
            "  FROM biz_cost_object t ",
            "  LEFT JOIN biz_material m ON m.id = t.product_id ",
            "  LEFT JOIN sys_dept d ON d.id = t.workshop_dept_id AND d.deleted_at IS NULL ",
            "  {}"
        ),
        joins_sql()
    )
}

/// 在制余额的 SQL 片段（`q` 为行集别名，派生表已算出 col_* / tr_*）。
///
/// 放在一处：`ORDER BY`、`only_wip` 过滤、表尾合计三处**必须**用同一个表达式，
/// 否则「按余额排的序」「筛出来的行」「加起来的合计」会各算一套，用户一眼就看出来不对。
fn balance_expr(alias: &str) -> String {
    format!(
        "({alias}.col_mat + {alias}.col_labor + {alias}.col_oh \
          - {alias}.tr_mat - {alias}.tr_labor - {alias}.tr_oh)"
    )
}

/// 业务过滤条件（**不含**数据范围片段、**不含**四个期间参数）。
fn business_where(filter: &WipFilter) -> (String, Vec<Value>) {
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
        sql.push_str(
            " AND (t.object_no LIKE ? OR COALESCE(t.source_doc_no, '') LIKE ? \
                OR COALESCE(t.lot_no, '') LIKE ? OR COALESCE(m.material_code, '') LIKE ? \
                OR COALESCE(m.material_name, '') LIKE ?)",
        );
        for _ in 0..5 {
            params.push(Value::Text(pattern.clone()));
        }
    }
    if filter.only_wip {
        // 「只看还有余额」用**行内表达式**，不能用 SELECT 别名（WHERE 看不到别名）。
        sql.push_str(
            " AND (COALESCE(c.mat_amount, 0) + COALESCE(c.labor_amount, 0) + COALESCE(c.oh_amount, 0) \
                - COALESCE(f.mat_amount, 0) - COALESCE(f.labor_amount, 0) - COALESCE(f.oh_amount, 0)) > 0",
        );
    }
    (sql, params)
}

/// 关键字 → LIKE 模式（`%` / `_` 转义）。
fn keyword_pattern(keyword: Option<&str>) -> Option<String> {
    keyword.map(str::trim).filter(|text| !text.is_empty()).map(|text| {
        format!(
            "%{}%",
            text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
        )
    })
}

/// 占位符 / 参数一致性校验（**有牙**：错位立刻 500，绝不绑错值算错账）。
fn bind(sql: &str, params: &[Value]) -> DbResult<()> {
    let placeholders = sql.matches('?').count();
    if placeholders != params.len() {
        return Err(DbError::InvalidValue(format!(
            "在制余额 SQL 占位符与参数数量不一致（?={placeholders}，参数={}）：参数错位会静默绑错值，\
             拒绝执行。SQL={sql}",
            params.len()
        )));
    }
    Ok(())
}

// ==================== 读：余额表 ====================

/// 在制余额表分页（数据范围 = 成本对象列表的同一份）。
///
/// # 参数顺序（文本顺序，**改 SQL 必须同步改这里**）
///
/// ```text
/// ① 累计归集期间  joins_sql 的 c.period_code <= ?   ← FROM 里的派生表，文本最前
/// ② 累计结转期间  joins_sql 的 f.period_code <= ?
/// ③ 本期投入期间  joins_sql 的 ci.period_code = ?
/// ④ 本期转出期间  joins_sql 的 fi.period_code = ?
/// ⑤ 数据范围      scope_filter.sql() 的 ?           ← 第一层 WHERE
/// ⑥ 业务条件      status / 车间 / 关键字 / only_wip ← 第一层 WHERE（片段之后）
/// ⑦ 分页          LIMIT ? OFFSET ?                  ← 只在数据页，count 没有
/// ```
pub async fn page_rows(principal: &ScopedPrincipal, filter: &WipFilter) -> DbResult<(Vec<WipRow>, i64)> {
    let scope_filter = scope::filter_for(principal, WIP_TARGET);
    let (biz_sql, biz_params) = business_where(filter);
    let base = format!(
        "{} WHERE t.deleted_at IS NULL {}{}",
        list_sql(),
        scope_filter.sql(),
        biz_sql
    );

    let mut params: Vec<Value> = Vec::with_capacity(4 + scope_filter.params().len() + biz_params.len());
    for _ in 0..4 {
        params.push(Value::Text(filter.period_code.clone()));
    }
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
    // ORDER BY 里的别名（col_mat 等）只在 `base` 这一层可见，所以排序键直接写在
    // `base` 的 SELECT 列表上；`balance` 排序用的是同一份派生表列。
    let offset = filter.page.saturating_sub(1).saturating_mul(filter.page_size);
    let mut page_params = params;
    page_params.push(Value::I64(i64::try_from(filter.page_size).unwrap_or(i64::MAX)));
    page_params.push(Value::I64(i64::try_from(offset).unwrap_or(i64::MAX)));
    let sql = format!("{base} {} LIMIT ? OFFSET ?", dto::order_by_sql(&filter.sort));
    bind(&sql, &page_params)?;
    let rows = db().query_sql(sql, page_params).await?;

    let list = rows.iter().map(row_of).collect::<DbResult<Vec<WipRow>>>()?;
    Ok((list, total))
}

/// 由查询行构造一条余额表行。
fn row_of(row: &Row) -> DbResult<WipRow> {
    let snapshot: Option<String> = row.get_typed("workshop_dept_name")?;
    let collected = Amounts {
        material: row.get_typed("col_mat")?,
        labor: row.get_typed("col_labor")?,
        overhead: row.get_typed("col_oh")?,
    };
    let transferred = Amounts {
        material: row.get_typed("tr_mat")?,
        labor: row.get_typed("tr_labor")?,
        overhead: row.get_typed("tr_oh")?,
    };
    if collected.material < Decimal::ZERO || collected.labor < Decimal::ZERO || collected.overhead < Decimal::ZERO {
        // 归集金额库层 `ck_biz_cost_collect_amount CHECK (amount >= 0)` 保证不会出现；
        // 出现就是有人绕过了约束，直接报错比把负数当余额算出去安全。
        return Err(DbError::InvalidValue(
            "在制余额：累计归集出现负值（库层 CHECK 被绕过？）".to_string(),
        ));
    }
    Ok(WipRow {
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
        collected,
        transferred,
        period_in: Amounts {
            material: row.get_typed("in_mat")?,
            labor: row.get_typed("in_labor")?,
            overhead: row.get_typed("in_oh")?,
        },
        period_out: Amounts {
            material: row.get_typed("out_mat")?,
            labor: row.get_typed("out_labor")?,
            overhead: row.get_typed("out_oh")?,
        },
        collect_lines: row.get_typed("col_lines")?,
        transfer_times: row.get_typed("tr_times")?,
        first_period: row.get_typed("first_period")?,
        last_period: row.get_typed("last_period")?,
        last_transfer_at: row.get_typed("last_at")?,
    })
}

/// 整张表的合计（与 [`page_rows`] 同一份筛选，**不受分页影响**）。
///
/// 参数顺序：① 四个期间（FROM 的派生表）② 数据范围 ③ 业务条件 —— 与行查询完全一致。
pub async fn totals(principal: &ScopedPrincipal, filter: &WipFilter) -> DbResult<WipTotals> {
    let scope_filter = scope::filter_for(principal, WIP_TARGET);
    let (biz_sql, biz_params) = business_where(filter);
    let base = format!(
        "{} WHERE t.deleted_at IS NULL {}{}",
        list_sql(),
        scope_filter.sql(),
        biz_sql
    );
    let sql = format!(
        concat!(
            "SELECT round(COALESCE(sum(q.col_mat), 0), 4) AS col_mat, ",
            "       round(COALESCE(sum(q.col_labor), 0), 4) AS col_labor, ",
            "       round(COALESCE(sum(q.col_oh), 0), 4) AS col_oh, ",
            "       round(COALESCE(sum(q.tr_mat), 0), 4) AS tr_mat, ",
            "       round(COALESCE(sum(q.tr_labor), 0), 4) AS tr_labor, ",
            "       round(COALESCE(sum(q.tr_oh), 0), 4) AS tr_oh, ",
            "       round(COALESCE(sum(q.in_mat), 0), 4) AS in_mat, ",
            "       round(COALESCE(sum(q.in_labor), 0), 4) AS in_labor, ",
            "       round(COALESCE(sum(q.in_oh), 0), 4) AS in_oh, ",
            "       round(COALESCE(sum(q.out_mat), 0), 4) AS out_mat, ",
            "       round(COALESCE(sum(q.out_labor), 0), 4) AS out_labor, ",
            "       round(COALESCE(sum(q.out_oh), 0), 4) AS out_oh, ",
            "       count(*) AS batch_count, ",
            "       count(*) FILTER (WHERE {} > 0) AS wip_batch_count, ",
            "       count(*) FILTER (WHERE q.col_mat < q.tr_mat OR q.col_labor < q.tr_labor ",
            "                          OR q.col_oh < q.tr_oh) AS negative_batch_count ",
            "  FROM ({}) AS q"
        ),
        balance_expr("q"),
        base
    );

    let mut params: Vec<Value> = Vec::with_capacity(4 + scope_filter.params().len() + biz_params.len());
    for _ in 0..4 {
        params.push(Value::Text(filter.period_code.clone()));
    }
    params.extend_from_slice(scope_filter.params());
    params.extend(biz_params);

    bind(&sql, &params)?;
    let row = db()
        .query_first_sql(sql, params)
        .await?
        .ok_or_else(|| DbError::InvalidValue("在制余额合计查询没有返回行（聚合必有一行）".to_string()))?;
    Ok(WipTotals {
        collected: Amounts {
            material: row.get_typed("col_mat")?,
            labor: row.get_typed("col_labor")?,
            overhead: row.get_typed("col_oh")?,
        },
        transferred: Amounts {
            material: row.get_typed("tr_mat")?,
            labor: row.get_typed("tr_labor")?,
            overhead: row.get_typed("tr_oh")?,
        },
        period_in: Amounts {
            material: row.get_typed("in_mat")?,
            labor: row.get_typed("in_labor")?,
            overhead: row.get_typed("in_oh")?,
        },
        period_out: Amounts {
            material: row.get_typed("out_mat")?,
            labor: row.get_typed("out_labor")?,
            overhead: row.get_typed("out_oh")?,
        },
        batch_count: row.get_typed("batch_count")?,
        wip_batch_count: row.get_typed("wip_batch_count")?,
        negative_batch_count: row.get_typed("negative_batch_count")?,
    })
}

// ==================== 读：台账 ====================
//
// 下面三个函数**不再**注入数据范围：调用方必须先用
// `cost_collect_sheet_model::find_visible_batch` 做过探针（不可见 ⇒ 404）。
// 在这里「顺手加个 scope」会与探针口径打架（`biz_cost_collect` 没有批次车间列，
// 费池行的 `dept_id` 是费用发生车间，不是批次的车间）。

/// 本批**逐期间**的归集流量（`period_code ≤ cutoff`，升序）。
pub async fn collect_flows(object_id: Uuid, cutoff: &str) -> DbResult<Vec<PeriodFlow>> {
    let sql = "SELECT period_code, \
                      round(COALESCE(sum(amount) FILTER (WHERE element = 'MATERIAL'), 0), 4) AS mat_amount, \
                      round(COALESCE(sum(amount) FILTER (WHERE element = 'LABOR'), 0), 4) AS labor_amount, \
                      round(COALESCE(sum(amount) FILTER (WHERE element = 'OVERHEAD'), 0), 4) AS oh_amount, \
                      count(*) AS lines \
                 FROM biz_cost_collect \
                WHERE cost_object_id = ?::uuid AND deleted_at IS NULL AND period_code <= ? \
                GROUP BY period_code \
                ORDER BY period_code";
    let rows = db()
        .query_sql(
            sql.to_string(),
            vec![Value::from(&object_id), Value::Text(cutoff.to_string())],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(PeriodFlow {
                period_code: row.get_typed("period_code")?,
                inflow: Amounts {
                    material: row.get_typed("mat_amount")?,
                    labor: row.get_typed("labor_amount")?,
                    overhead: row.get_typed("oh_amount")?,
                },
                collect_lines: row.get_typed("lines")?,
                outflow: Amounts::default(),
                transfer_times: 0,
            })
        })
        .collect()
}

/// 本批**逐期间**的结转流量（`period_code ≤ cutoff`，升序）。
pub async fn finish_flows(object_id: Uuid, cutoff: &str) -> DbResult<Vec<PeriodFlow>> {
    let sql = "SELECT period_code, \
                      round(COALESCE(sum(material_amount), 0), 4) AS mat_amount, \
                      round(COALESCE(sum(labor_amount), 0), 4) AS labor_amount, \
                      round(COALESCE(sum(overhead_amount), 0), 4) AS oh_amount, \
                      count(*) AS times \
                 FROM biz_cost_batch_finish \
                WHERE object_id = ?::uuid AND deleted_at IS NULL AND period_code <= ? \
                GROUP BY period_code \
                ORDER BY period_code";
    let rows = db()
        .query_sql(
            sql.to_string(),
            vec![Value::from(&object_id), Value::Text(cutoff.to_string())],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(PeriodFlow {
                period_code: row.get_typed("period_code")?,
                inflow: Amounts::default(),
                collect_lines: 0,
                outflow: Amounts {
                    material: row.get_typed("mat_amount")?,
                    labor: row.get_typed("labor_amount")?,
                    overhead: row.get_typed("oh_amount")?,
                },
                transfer_times: row.get_typed("times")?,
            })
        })
        .collect()
}

/// 截止期间**之后**还有多少归集 / 结转（`None` = 之后一片空白）。
///
/// 台账是「截至 P」的快照：P 之后发生的成本不能算进 P 的余额，但也不能假装不存在
/// —— 界面上要说「之后还有 ¥X 的归集」，否则用户会以为数据丢了。
pub async fn later_activity(object_id: Uuid, cutoff: &str) -> DbResult<Option<(Decimal, Decimal)>> {
    let sql = "SELECT \
                 (SELECT COALESCE(sum(amount), 0) FROM biz_cost_collect \
                   WHERE cost_object_id = ?::uuid AND deleted_at IS NULL AND period_code > ?) AS later_collect, \
                 (SELECT COALESCE(sum(total_amount), 0) FROM biz_cost_batch_finish \
                   WHERE object_id = ?::uuid AND deleted_at IS NULL AND period_code > ?) AS later_transfer";
    let row = db()
        .query_first_sql(
            sql.to_string(),
            vec![
                Value::from(&object_id),
                Value::Text(cutoff.to_string()),
                Value::from(&object_id),
                Value::Text(cutoff.to_string()),
            ],
        )
        .await?
        .ok_or_else(|| DbError::InvalidValue("之后发生额查询没有返回行（标量子查询必有一行）".to_string()))?;
    let collected: Decimal = row.get_typed("later_collect")?;
    let transferred: Decimal = row.get_typed("later_transfer")?;
    if collected.is_zero() && transferred.is_zero() {
        Ok(None)
    } else {
        Ok(Some((collected, transferred)))
    }
}

/// 最新一条已结算计算单（期间结算口径，`period_code ≤ cutoff`）。
pub async fn settled_item(object_id: Uuid, cutoff: &str) -> DbResult<Option<SettledFacts>> {
    let sql = "SELECT p.period_code, i.finished_cost, i.wip_end_cost \
                 FROM biz_cost_period_item i \
                 JOIN biz_cost_period p ON p.id = i.period_id \
                WHERE i.cost_object_id = ?::uuid AND i.status = 'SETTLED' \
                  AND p.deleted_at IS NULL AND p.period_code <= ? \
                ORDER BY p.period_code DESC \
                LIMIT 1";
    let rows = db()
        .query_sql(
            sql.to_string(),
            vec![Value::from(&object_id), Value::Text(cutoff.to_string())],
        )
        .await?;
    rows.first()
        .map(|row| {
            Ok(SettledFacts {
                period_code: row.get_typed("period_code")?,
                finished_cost: row.get_typed("finished_cost")?,
                wip_end_cost: row.get_typed("wip_end_cost")?,
            })
        })
        .transpose()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 参数错位必须 500：`?` 个数与参数个数不等 ⇒ 拒绝执行（绝不静默绑错值）。
    #[test]
    fn bind_rejects_placeholder_mismatch() {
        let sql = "SELECT 1 WHERE a <= ? AND b = ?";
        assert!(bind(sql, &[Value::Text("x".to_string()), Value::Text("y".to_string())]).is_ok());
        assert!(bind(sql, &[Value::Text("x".to_string())]).is_err());
    }

    /// 四段派生表 + 数据页 = 至少 4 个期间占位符（改 SQL 忘了同步参数会当场红），
    /// 而且「累计式 vs 当期式」的边界必须逐个对齐（把某个 `<=` 写成 `=` 就是「余额
    /// 只剩本月」，账会静默少算 —— 真库用例 V1 也钉了这一条，这里从 SQL 文本再钉一遍）。
    #[test]
    fn list_sql_has_exactly_four_period_placeholders() {
        let sql = list_sql();
        assert_eq!(
            sql.matches('?').count(),
            4,
            "累计归集 / 累计结转 / 本期投入 / 本期转出 各一处期间参数"
        );
        for cumulative in ["c.period_code <= ?", "f.period_code <= ?"] {
            assert!(
                sql.contains(cumulative),
                "累计式必须用「≤ 截止期间」（缺 {cumulative}）"
            );
        }
        for current in ["ci.period_code = ?", "fi.period_code = ?"] {
            assert!(sql.contains(current), "当期式必须用「= 截止期间」（缺 {current}）");
        }
        assert_eq!(sql.matches("period_code <= ?").count(), 2, "只有两处累计式");
        assert_eq!(sql.matches("period_code = ?").count(), 2, "只有两处当期式");
    }

    /// 排序与合计必须用同一个余额表达式（不同就是「排序 / 筛选 / 合计」三套口径）。
    #[test]
    fn order_by_balance_covers_the_same_six_columns_as_the_totals_expression() {
        let order = dto::order_by_sql(dto::SORT_BALANCE);
        let expr = balance_expr("q");
        for column in ["col_mat", "col_labor", "col_oh"] {
            assert!(expr.contains(column), "合计表达式缺 {column}：{expr}");
        }
        for column in ["tr_mat", "tr_labor", "tr_oh"] {
            assert!(expr.contains(column), "合计表达式缺 {column}：{expr}");
        }
        // 行查询里的列名（c./f. 派生表别名）与合计里的别名一一对应
        for (row_name, total_name) in [
            ("c.mat_amount", "col_mat"),
            ("c.labor_amount", "col_labor"),
            ("c.oh_amount", "col_oh"),
            ("f.mat_amount", "tr_mat"),
            ("f.labor_amount", "tr_labor"),
            ("f.oh_amount", "tr_oh"),
        ] {
            assert!(order.contains(row_name), "余额排序缺 {row_name}：{order}");
            assert!(expr.contains(total_name), "合计表达式缺 {total_name}");
            assert!(
                list_sql().contains(&format!("AS {total_name}")),
                "行查询缺别名 {total_name}"
            );
        }
    }

    #[test]
    fn only_wip_filter_uses_the_same_expression() {
        let filter = WipFilter {
            period_code: "2026-11".to_string(),
            keyword: None,
            workshop_dept_id: None,
            status: Some("OPEN".to_string()),
            only_wip: true,
            sort: dto::SORT_BALANCE.to_string(),
            page: 1,
            page_size: 20,
        };
        let (sql, params) = business_where(&filter);
        assert!(sql.contains("status = ?") || sql.contains("t.status = ?"), "{sql}");
        assert!(sql.contains("COALESCE(c.mat_amount, 0)"), "only_wip 必须用行内表达式");
        // status + only_wip 不引入额外参数（only_wip 是常量表达式）
        assert_eq!(params.len(), 1, "只有 status 需要绑值");
    }

    #[test]
    fn keyword_pattern_escapes_wildcards() {
        assert_eq!(keyword_pattern(Some("  50%_x  ")), Some("%50\\%\\_x%".to_string()));
        assert_eq!(keyword_pattern(Some("   ")), None);
        assert_eq!(keyword_pattern(None), None);
    }
}

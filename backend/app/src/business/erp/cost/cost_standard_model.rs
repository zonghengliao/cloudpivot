//! 标准成本卡 **取数层**（成本核算 · M5-08 · 表 `biz_cost_standard` / `biz_cost_standard_item`）。
//!
//! 本文件只做「读原语 + 三个写入原语」，不含业务判据：重叠 / 已生效锁定 / 删除纪律在
//! [`cost_standard_handler`](crate::business::erp::cost::cost_standard_handler)，
//! 值域与纯逻辑在 [`cost_standard_dto`](crate::business::erp::cost::cost_standard_dto)。
//!
//! # 读的东西
//!
//! | 读 | 干什么 |
//! |---|---|
//! | [`find_by_id`] / [`find_by_id_in`] | 单版详情（卡头 + 物料实时 join + 明细行数） |
//! | [`versions_of`] | 某物料**全部存活版本**（重叠判定与版本号 max+1 的输入） |
//! | [`find_covering_in`] | 某物料某天**生效**的那一版（期间取数的落点） |
//! | [`page`] | 版本列表（物料 / 状态 / 期间 / 日期 / 关键字） |
//! | [`period_sheet`] | 期间 × 物料 矩阵（期间末生效版本 + 期间内是否换过版） |
//! | [`items_of`] | 标准 BOM（明细 + 耗用物料实时 join） |
//!
//! # 为什么列表/矩阵都走 `scope::paginate_unscoped`
//!
//! 标准成本卡是**全公司财务口径配置**（与 `biz_cost_method_config` 同类）：它不归属任何
//! 部门或业务员，物料本身也不是部门数据。本表未登记进 `SCOPED_TABLES`，分页只允许走
//! `paginate_unscoped`（`scope::paginate` 会因锚点是 unscoped 直接报错）。
//!
//! # 名字怎么给（AGENTS 约定 13）
//!
//! `material_id` / `component_material_id` 是**主数据关联** ⇒ 实时 join `biz_material`
//! 取 `material_code` / `material_name`：物料改名后这里就该显示新名。
//! 卡头不存任何物料名快照（版本行是「口径」不是「历史单据」，历史口径靠版本号 + 生效区间
//! 追溯，不需要名字快照）。
//!
//! # 并发
//!
//! 「读现有版本 → 判重叠 → 写」是读后写，两个并发请求可能双双通过预检。两层兜底：
//! ① [`lock_material`]：事务内按物料取 `pg_advisory_xact_lock`，同物料的写入串行化；
//! ② 库层 `ex_biz_cost_std_active_window`（V0463，EXCLUDE）作最后一道物理防线。
//! 正常路径上 ① 就能给出可读的 409，② 只在「有人绕过 handler 直插」时才响。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_standard_dto::{self as dto, Amounts, StandardItemSpec, StandardSpec};
use cloudpivot_kernel::business::shared::scope;

/// 卡头表。
pub const TABLE_HEAD: &str = "biz_cost_standard";
/// 明细表。
pub const TABLE_ITEM: &str = "biz_cost_standard_item";

/// 分页锚点（**SCOPE-EXEMPT**：全公司财务口径配置，不是某部门的业务单据）。
pub const TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(TABLE_HEAD);

/// 稳定排序：启用在前 → 生效日倒序（最新一版在最上）→ 物料编码 → id（同秒插入也稳定）。
pub const ORDER: &str = "ORDER BY (t.status = 1) DESC, t.effective_from DESC, m.material_code ASC NULLS LAST, t.id ASC";

/// 卡头列（与 [`StandardRow`] 字段一一对应；物料派生列与明细行数在 `select_sql` 里另加）。
const COLUMNS: &str = "id, material_id, version_no, effective_from, effective_to, \
                       std_material_cost, std_labor_cost, std_overhead_cost, std_total_cost, \
                       std_labor_hours, std_labor_rate, status, remark, created_at, updated_at";

/// 明细列（与 [`ItemRow`] 字段一一对应）。
const ITEM_COLUMNS: &str = "i.id, i.line_no, i.component_material_id, i.std_qty, i.std_price, i.std_amount";

// ==================== 行结构 ====================

/// `biz_cost_standard` 一行 + 实时 join 出来的物料信息 + 明细行数。
#[derive(Debug, Clone)]
pub struct StandardRow {
    /// 主键。
    pub id: Uuid,
    /// 成品 / 半成品物料。
    pub material_id: Uuid,
    /// 版本号（同物料内递增）。
    pub version_no: i32,
    /// 生效起始日。
    pub effective_from: NaiveDate,
    /// 停止日。
    pub effective_to: Option<NaiveDate>,
    /// 单位标准材料成本。
    pub std_material_cost: Decimal,
    /// 单位标准人工成本。
    pub std_labor_cost: Decimal,
    /// 单位标准制造费用。
    pub std_overhead_cost: Decimal,
    /// 单位标准总成本。
    pub std_total_cost: Decimal,
    /// 单位标准工时。
    pub std_labor_hours: Decimal,
    /// 标准工资率。
    pub std_labor_rate: Decimal,
    /// 状态（1 / 0）。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 物料编码（实时 join）。
    pub material_code: Option<String>,
    /// 物料名称（实时 join）。
    pub material_name: Option<String>,
    /// 物料类型（实时 join）。
    pub material_type: Option<String>,
    /// 物料规格（实时 join）。
    pub material_spec: Option<String>,
    /// 标准 BOM 行数。
    pub items_count: i64,
}

impl StandardRow {
    /// 由查询行构造（列名见 [`COLUMNS`] + 5 个派生列）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            material_id: row.get_typed("material_id")?,
            version_no: row.get_typed("version_no")?,
            effective_from: row.get_typed("effective_from")?,
            effective_to: row.get_typed("effective_to")?,
            std_material_cost: row.get_typed("std_material_cost")?,
            std_labor_cost: row.get_typed("std_labor_cost")?,
            std_overhead_cost: row.get_typed("std_overhead_cost")?,
            std_total_cost: row.get_typed("std_total_cost")?,
            std_labor_hours: row.get_typed("std_labor_hours")?,
            std_labor_rate: row.get_typed("std_labor_rate")?,
            status: row.get_typed("status")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            material_code: row.get_typed("material_code")?,
            material_name: row.get_typed("material_name")?,
            material_type: row.get_typed("material_type")?,
            material_spec: row.get_typed("material_spec")?,
            items_count: row.get_typed("items_count")?,
        })
    }

    /// 判定用的纯数据投影。
    pub fn to_view(&self) -> dto::VersionView {
        dto::VersionView {
            id: self.id,
            material_id: self.material_id,
            version_no: self.version_no,
            effective_from: self.effective_from,
            effective_to: self.effective_to,
            status: self.status,
        }
    }

    /// 当前值 → 归一化规格（PUT 合并用；明细由 [`items_of`] 另取）。
    pub fn to_spec(&self, items: Vec<StandardItemSpec>) -> StandardSpec {
        StandardSpec {
            material_id: self.material_id,
            effective_from: self.effective_from,
            effective_to: self.effective_to,
            std_labor_hours: self.std_labor_hours,
            std_labor_rate: self.std_labor_rate,
            std_overhead_cost: self.std_overhead_cost,
            status: self.status,
            remark: self.remark.clone(),
            items,
        }
    }

    /// 报文里的「谁」：物料编码 + 名称，物料被删 / 缺失时回落 id。
    pub fn material_label(&self) -> String {
        match (&self.material_code, &self.material_name) {
            (Some(code), Some(name)) => format!("{code}（{name}）"),
            (Some(code), None) => code.clone(),
            _ => self.material_id.to_string(),
        }
    }
}

/// `biz_cost_standard_item` 一行 + 耗用物料实时 join。
#[derive(Debug, Clone)]
pub struct ItemRow {
    /// 明细主键。
    pub id: Uuid,
    /// 行号。
    pub line_no: i32,
    /// 耗用物料。
    pub component_material_id: Uuid,
    /// 耗用物料编码（实时 join）。
    pub component_material_code: Option<String>,
    /// 耗用物料名称（实时 join）。
    pub component_material_name: Option<String>,
    /// 单位标准用量。
    pub std_qty: Decimal,
    /// 标准单价。
    pub std_price: Decimal,
    /// 标准金额。
    pub std_amount: Decimal,
}

impl ItemRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            line_no: row.get_typed("line_no")?,
            component_material_id: row.get_typed("component_material_id")?,
            component_material_code: row.get_typed("component_material_code")?,
            component_material_name: row.get_typed("component_material_name")?,
            std_qty: row.get_typed("std_qty")?,
            std_price: row.get_typed("std_price")?,
            std_amount: row.get_typed("std_amount")?,
        })
    }

    /// → 规格里的明细（PUT 合并时把库里的明细当成「当前值」）。
    pub fn to_spec(&self) -> StandardItemSpec {
        StandardItemSpec {
            component_material_id: self.component_material_id,
            std_qty: self.std_qty,
            std_price: self.std_price,
        }
    }
}

/// 期间矩阵的一行（版本 + 期间内是否换过版）。
#[derive(Debug, Clone)]
pub struct PeriodSheetRow {
    /// 版本行。
    pub row: StandardRow,
    /// 该期间内是否还有别的启用版本生效过。
    pub switch_in_period: bool,
}

/// 物料引用（校验 `material_id` / 明细 `component_material_id` 用）。
#[derive(Debug, Clone)]
pub struct MaterialRef {
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 物料类型。
    pub material_type: String,
    /// 状态（1 / 0）。
    pub status: i16,
}

impl MaterialRef {
    /// 报文里的物料标称。
    pub fn label(&self) -> String {
        format!("{}（{}）", self.material_code, self.material_name)
    }
}

/// 列表筛选（handler 归一化后交过来；`page/page_size` 已过 `normalize_page`）。
#[derive(Debug, Clone, Default)]
pub struct StandardFilter {
    /// 物料。
    pub material_id: Option<Uuid>,
    /// 状态。
    pub status: Option<i16>,
    /// 期间区间（版本生效区间与它有交集即命中）。
    pub period: Option<(NaiveDate, NaiveDate)>,
    /// 某一天（版本覆盖该日即命中）。
    pub on_date: Option<NaiveDate>,
    /// 关键字（物料编码 / 物料名称 / 备注）。
    pub keyword: Option<String>,
    /// 页号。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 期间矩阵筛选。
#[derive(Debug, Clone, Default)]
pub struct PeriodSheetFilter {
    /// 期间区间（必填，handler 已解析）。
    pub period: Option<(NaiveDate, NaiveDate)>,
    /// 关键字（物料编码 / 物料名称）。
    pub keyword: Option<String>,
    /// 页号。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

// ==================== 读 ====================

/// 卡头 + 物料派生列 + 明细行数的公共 SELECT。
fn select_sql() -> String {
    let columns: Vec<String> = COLUMNS
        .split(',')
        .map(|column| format!("t.{}", column.trim()))
        .collect();
    format!(
        "SELECT {}, \
                m.material_code AS material_code, \
                m.material_name AS material_name, \
                m.material_type AS material_type, \
                m.spec          AS material_spec, \
                (SELECT count(*) FROM {TABLE_ITEM} i WHERE i.standard_id = t.id)::bigint AS items_count \
           FROM {TABLE_HEAD} t \
           LEFT JOIN biz_material m ON m.id = t.material_id AND m.deleted_at IS NULL",
        columns.join(", ")
    )
}

/// 按主键取存活行（软删 / 不存在 → `None`）。
pub async fn find_by_id(id: Uuid) -> DbResult<Option<StandardRow>> {
    find_by_id_in(db(), id).await
}

/// 按主键取存活行（事务内版本：写完之后回查走它，读到的是自己刚写的那一行）。
pub async fn find_by_id_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<StandardRow>> {
    let sql = format!("{} WHERE t.id = ?::uuid AND t.deleted_at IS NULL", select_sql());
    let rows = ex.query_sql(sql, vec![Value::from(&id)]).await?;
    rows.first().map(StandardRow::from_row).transpose()
}

/// 某物料的**全部存活版本**（启用 + 失效，按生效日升序）。
///
/// 重叠判定与「版本号 = max + 1」都吃这一份，不另发两条查询（同一快照里读，避免自己打架）。
pub async fn versions_of(ex: &impl SqlExecutor, material_id: Uuid) -> DbResult<Vec<StandardRow>> {
    let sql = format!(
        "{} WHERE t.material_id = ?::uuid AND t.deleted_at IS NULL ORDER BY t.effective_from ASC, t.id ASC",
        select_sql()
    );
    let rows = ex.query_sql(sql, vec![Value::from(&material_id)]).await?;
    rows.iter().map(StandardRow::from_row).collect()
}

/// 某物料在某天**生效**的那一版（启用 + 覆盖该日）。
///
/// 库层 `ex_biz_cost_std_active_window` 保证至多一行；这里仍取 `first()` 并交由调用方
/// 处理 `None`（期间末没有生效版本是真实的业务状态，不是错误）。
pub async fn find_covering_in(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    date: NaiveDate,
) -> DbResult<Option<StandardRow>> {
    let sql = format!(
        "{} WHERE t.material_id = ?::uuid AND t.deleted_at IS NULL AND t.status = 1 \
           AND t.effective_from <= ?::text::date \
           AND (t.effective_to IS NULL OR t.effective_to >= ?::text::date) \
         ORDER BY t.effective_from DESC LIMIT 1",
        select_sql()
    );
    let rows = ex
        .query_sql(
            sql,
            vec![
                Value::from(&material_id),
                Value::Text(date.to_string()),
                Value::Text(date.to_string()),
            ],
        )
        .await?;
    rows.first().map(StandardRow::from_row).transpose()
}

/// 某物料在 `[start, end]`（期间）内也生效过的**其它**启用版本（切换提示用；不含 `exclude`）。
pub async fn other_versions_in_window(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    start: NaiveDate,
    end: NaiveDate,
    exclude: Uuid,
) -> DbResult<Vec<StandardRow>> {
    let sql = format!(
        "{} WHERE t.material_id = ?::uuid AND t.deleted_at IS NULL AND t.status = 1 AND t.id <> ?::uuid \
           AND t.effective_from <= ?::text::date \
           AND (t.effective_to IS NULL OR t.effective_to >= ?::text::date) \
         ORDER BY t.effective_from ASC, t.id ASC",
        select_sql()
    );
    let rows = ex
        .query_sql(
            sql,
            vec![
                Value::from(&material_id),
                Value::from(&exclude),
                Value::Text(end.to_string()),
                Value::Text(start.to_string()),
            ],
        )
        .await?;
    rows.iter().map(StandardRow::from_row).collect()
}

/// 标准 BOM（按行号升序）。
pub async fn items_of(ex: &impl SqlExecutor, standard_id: Uuid) -> DbResult<Vec<ItemRow>> {
    let sql = format!(
        "SELECT {ITEM_COLUMNS}, \
                m.material_code AS component_material_code, \
                m.material_name AS component_material_name \
           FROM {TABLE_ITEM} i \
           LEFT JOIN biz_material m ON m.id = i.component_material_id AND m.deleted_at IS NULL \
          WHERE i.standard_id = ?::uuid \
          ORDER BY i.line_no ASC"
    );
    let rows = ex.query_sql(sql, vec![Value::from(&standard_id)]).await?;
    rows.iter().map(ItemRow::from_row).collect()
}

/// 物料是否存在（未软删）→ 编码 / 名称 / 类型 / 状态。
pub async fn material_of(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<MaterialRef>> {
    let rows = ex
        .query_sql(
            "SELECT material_code, material_name, material_type, status \
               FROM biz_material WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(material_ref_of).transpose()
}

/// 批量取物料（标准 BOM 校验用：一次问库，不做逐行 N+1）。
///
/// 返回「存在的那些」，缺失的由调用方点名单行号 —— 缺失是真实的录入错误，
/// 静默过滤掉会让标准 BOM 少一行而金额照算，比报错坏得多。
pub async fn materials_of(ex: &impl SqlExecutor, ids: &[Uuid]) -> DbResult<Vec<(Uuid, MaterialRef)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = ex
        .query_sql(
            "SELECT id, material_code, material_name, material_type, status \
               FROM biz_material WHERE id = ANY(?::text::uuid[]) AND deleted_at IS NULL"
                .to_string(),
            vec![crate::business::erp::model_base::ids_array_param(ids)],
        )
        .await?;
    rows.iter()
        .map(|row| Ok((row.get_typed("id")?, material_ref_of(row)?)))
        .collect()
}

/// 物料行 → [`MaterialRef`]。
fn material_ref_of(row: &Row) -> DbResult<MaterialRef> {
    Ok(MaterialRef {
        material_code: row.get_typed("material_code")?,
        material_name: row.get_typed("material_name")?,
        material_type: row.get_typed("material_type")?,
        status: row.get_typed("status")?,
    })
}

/// 版本列表分页（SCOPE-EXEMPT，走 `scope::paginate_unscoped`）。
pub async fn page(filter: &StandardFilter) -> DbResult<(Vec<StandardRow>, i64)> {
    let mut sql = format!("{} WHERE t.deleted_at IS NULL", select_sql());
    let mut params: Vec<Value> = Vec::new();
    if let Some(material_id) = filter.material_id {
        sql.push_str(" AND t.material_id = ?::uuid");
        params.push(Value::from(&material_id));
    }
    if let Some(status) = filter.status {
        sql.push_str(" AND t.status = ?");
        params.push(Value::I16(status));
    }
    if let Some((start, end)) = filter.period {
        sql.push_str(
            " AND t.effective_from <= ?::text::date \
              AND (t.effective_to IS NULL OR t.effective_to >= ?::text::date)",
        );
        params.push(Value::Text(end.to_string()));
        params.push(Value::Text(start.to_string()));
    }
    if let Some(on_date) = filter.on_date {
        sql.push_str(
            " AND t.effective_from <= ?::text::date \
              AND (t.effective_to IS NULL OR t.effective_to >= ?::text::date)",
        );
        params.push(Value::Text(on_date.to_string()));
        params.push(Value::Text(on_date.to_string()));
    }
    if let Some(pattern) = like_pattern(&filter.keyword) {
        sql.push_str(
            " AND (COALESCE(m.material_code, '') LIKE ? \
               OR COALESCE(m.material_name, '') LIKE ? \
               OR COALESCE(t.remark, '') LIKE ?)",
        );
        for _ in 0..3 {
            params.push(Value::from(pattern.clone()));
        }
    }
    let result = scope::paginate_unscoped(TARGET, &sql, params, Some(ORDER), filter.page, filter.page_size).await?;
    let rows = result
        .list
        .iter()
        .map(StandardRow::from_row)
        .collect::<DbResult<Vec<StandardRow>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 期间矩阵 SQL（拆出来便于单测形状；`has_keyword` 决定是否多两个 LIKE 占位符）。
///
/// 结构：卡头列照 [`select_sql`]，SELECT 列表里多一个 `switch_in_period`（期间内还有别的
/// 启用版本 ⇒ true），WHERE 限「启用 + 覆盖期间末」。
///
/// **占位符顺序 = 文本顺序**：先把 EXISTS 里两个日期参数（期末 / 期初 —— 判「期间内还有别的
/// 版本生效过」），再是 WHERE 里两个日期参数（**期末 / 期末** —— 判「覆盖期间末」），最后可选
/// 的两个 LIKE。改 SQL 必须同步改 [`period_sheet`] 的 push 顺序，否则日期与关键字会互相错位
/// （这类错位不报错、只出错数）。
pub(crate) fn period_sheet_sql(has_keyword: bool) -> String {
    let base = select_sql();
    // 按 `FROM {TABLE_HEAD} t ` 切（**不能**按 " FROM " 切：items_count 子查询里也有 FROM）
    let marker = format!(" FROM {TABLE_HEAD} t ");
    let (head, tail) = base
        .split_once(&marker)
        .expect("select_sql 必须含「 FROM biz_cost_standard t 」");
    let mut sql = format!(
        "{head}, \
                CASE WHEN EXISTS (SELECT 1 FROM {TABLE_HEAD} x \
                                   WHERE x.deleted_at IS NULL AND x.status = 1 \
                                     AND x.material_id = t.material_id AND x.id <> t.id \
                                     AND x.effective_from <= ?::text::date \
                                     AND (x.effective_to IS NULL OR x.effective_to >= ?::text::date)) \
                     THEN TRUE ELSE FALSE END AS switch_in_period \
           FROM {TABLE_HEAD} t {tail} \
          WHERE t.deleted_at IS NULL AND t.status = 1 \
            AND t.effective_from <= ?::text::date \
            AND (t.effective_to IS NULL OR t.effective_to >= ?::text::date)"
    );
    if has_keyword {
        sql.push_str(" AND (COALESCE(m.material_code, '') LIKE ? OR COALESCE(m.material_name, '') LIKE ?)");
    }
    sql
}

/// 期间 × 物料 矩阵：**期间末生效**的启用版本一行，附「期间内是否换过版」。
///
/// # 为什么取期间末而不是期间初
///
/// 差异分析（M5-11 起）按期末产量 × 标准单位成本结账，取数口径天然是「期末生效的那一版」。
/// 期间中途换版时不静默：`switch_in_period = true`，界面据此提示「本月换过标准」。
/// 该期间内还有哪些版本生效过，由 [`other_versions_in_window`] 给出。
pub async fn period_sheet(filter: &PeriodSheetFilter) -> DbResult<(Vec<PeriodSheetRow>, i64)> {
    let Some((start, end)) = filter.period else {
        return Ok((Vec::new(), 0));
    };
    let pattern = like_pattern(&filter.keyword);
    let sql = period_sheet_sql(pattern.is_some());
    // 占位符顺序见 `period_sheet_sql` 的文档：
    // EXISTS(期末, 期初) → WHERE(期末, 期末) → 关键字 ×2
    let mut params: Vec<Value> = vec![
        Value::Text(end.to_string()),
        Value::Text(start.to_string()),
        Value::Text(end.to_string()),
        Value::Text(end.to_string()),
    ];
    if let Some(pattern) = pattern {
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    let result = scope::paginate_unscoped(TARGET, &sql, params, Some(ORDER), filter.page, filter.page_size).await?;
    let mut rows = Vec::with_capacity(result.list.len());
    for row in &result.list {
        rows.push(PeriodSheetRow {
            row: StandardRow::from_row(row)?,
            switch_in_period: row.get_typed("switch_in_period")?,
        });
    }
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

// ==================== 写 ====================

/// 事务内按物料取 advisory lock（同物料的标准成本写入串行化）。
///
/// 键 = `hashtextextended(material_id::text, 0)`：同物料恒定同键，不同物料互不阻塞。
/// 用 `pg_advisory_xact_lock`（事务级）：提交 / 回滚自动释放，不会漏锁。
pub async fn lock_material(ex: &impl SqlExecutor, material_id: Uuid) -> DbResult<()> {
    // 注意：`pg_advisory_xact_lock` 返回 `void`，直接 SELECT 它会让驱动在反序列化第 0 列时
    // 报 "error deserializing column 0"。所以套一层子查询、外层只取常量 1 —— 锁照样在事务
    // 提交/回滚时释放，读回来的值与业务无关。
    ex.query_sql(
        "SELECT 1 AS locked \
           FROM (SELECT pg_advisory_xact_lock(hashtextextended(?::text, 0))) AS advisory_lock"
            .to_string(),
        vec![Value::Text(material_id.to_string())],
    )
    .await
    .map(|_| ())
}

/// 下一个版本号（同物料存活版本 max + 1）。
pub fn next_version_no(rows: &[StandardRow]) -> i32 {
    rows.iter().map(|row| row.version_no).max().unwrap_or(0) + 1
}

/// 建卡头 SQL。
pub(crate) fn insert_head_sql() -> String {
    format!(
        "INSERT INTO {TABLE_HEAD} \
             (id, material_id, version_no, effective_from, effective_to, \
              std_material_cost, std_labor_cost, std_overhead_cost, std_total_cost, \
              std_labor_hours, std_labor_rate, status, remark, \
              created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?::uuid, ?, ?::text::date, NULLIF(?::text,'')::text::date, \
                 ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                 ?::text::numeric, ?::text::numeric, ?, NULLIF(?::text,''), \
                 now(), now(), NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
    )
}

/// 改卡头 SQL（**只改未删除的行**；版本号 / 物料 / 生效日都可能被未生效版本的改写动，故全列可改）。
pub(crate) fn update_head_sql() -> String {
    format!(
        "UPDATE {TABLE_HEAD} SET material_id = ?::uuid, effective_from = ?::text::date, \
             effective_to = NULLIF(?::text,'')::text::date, \
             std_material_cost = ?::text::numeric, std_labor_cost = ?::text::numeric, \
             std_overhead_cost = ?::text::numeric, std_total_cost = ?::text::numeric, \
             std_labor_hours = ?::text::numeric, std_labor_rate = ?::text::numeric, \
             status = ?, remark = NULLIF(?::text,''), updated_at = now(), \
             update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL"
    )
}

/// 软删 SQL。
pub(crate) fn soft_delete_sql() -> String {
    format!(
        "UPDATE {TABLE_HEAD} SET deleted_at = now(), updated_at = now(), \
             update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL"
    )
}

/// 置状态 SQL（生效 / 失效）。
pub(crate) fn set_status_sql() -> String {
    format!(
        "UPDATE {TABLE_HEAD} SET status = ?, remark = NULLIF(?::text,''), \
             updated_at = now(), update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL"
    )
}

/// 插入明细 SQL（行号由调用方按数组顺序给）。
pub(crate) fn insert_item_sql() -> String {
    format!(
        "INSERT INTO {TABLE_ITEM} \
             (id, standard_id, line_no, component_material_id, std_qty, std_price, std_amount, created_at) \
         VALUES (?::uuid, ?::uuid, ?, ?::uuid, ?::text::numeric, ?::text::numeric, ?::text::numeric, now())"
    )
}

/// 建卡头（字段合法性已由 dto + handler 校验；金额由 dto 算好）。
pub async fn insert_head(
    ex: &impl SqlExecutor,
    id: Uuid,
    version_no: i32,
    spec: &StandardSpec,
    amounts: &Amounts,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    ex.exec_sql(
        insert_head_sql(),
        insert_head_params(id, version_no, spec, amounts, operator),
    )
    .await
}

/// 建卡头参数（与 [`insert_head_sql`] 的 `?` 顺序逐位对应）。
pub(crate) fn insert_head_params(
    id: Uuid,
    version_no: i32,
    spec: &StandardSpec,
    amounts: &Amounts,
    operator: Option<Uuid>,
) -> Vec<Value> {
    let operator = Value::Text(operator.map(|id| id.to_string()).unwrap_or_default());
    vec![
        Value::from(&id),
        Value::from(&spec.material_id),
        Value::I32(version_no),
        Value::Text(spec.effective_from.to_string()),
        Value::Text(spec.effective_to.map(|date| date.to_string()).unwrap_or_default()),
        Value::Text(amounts.material.to_string()),
        Value::Text(amounts.labor.to_string()),
        Value::Text(amounts.overhead.to_string()),
        Value::Text(amounts.total().to_string()),
        Value::Text(spec.std_labor_hours.to_string()),
        Value::Text(spec.std_labor_rate.to_string()),
        Value::I16(spec.status),
        Value::Text(spec.remark.clone().unwrap_or_default()),
        operator.clone(),
        operator,
    ]
}

/// 改卡头（金额同样由 dto 按新明细重算后传入）。返回受影响行数。
pub async fn update_head(
    ex: &impl SqlExecutor,
    id: Uuid,
    spec: &StandardSpec,
    amounts: &Amounts,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    ex.exec_sql(update_head_sql(), update_head_params(id, spec, amounts, operator))
        .await
}

/// 改卡头参数（与 [`update_head_sql`] 的 `?` 顺序逐位对应）。
pub(crate) fn update_head_params(
    id: Uuid,
    spec: &StandardSpec,
    amounts: &Amounts,
    operator: Option<Uuid>,
) -> Vec<Value> {
    vec![
        Value::from(&spec.material_id),
        Value::Text(spec.effective_from.to_string()),
        Value::Text(spec.effective_to.map(|date| date.to_string()).unwrap_or_default()),
        Value::Text(amounts.material.to_string()),
        Value::Text(amounts.labor.to_string()),
        Value::Text(amounts.overhead.to_string()),
        Value::Text(amounts.total().to_string()),
        Value::Text(spec.std_labor_hours.to_string()),
        Value::Text(spec.std_labor_rate.to_string()),
        Value::I16(spec.status),
        Value::Text(spec.remark.clone().unwrap_or_default()),
        Value::Text(operator.map(|id| id.to_string()).unwrap_or_default()),
        Value::from(&id),
    ]
}

/// 明细整表替换（先删后插；标准 BOM 是「这一版的组成」，没有逐行改的语义）。
pub async fn replace_items(ex: &impl SqlExecutor, standard_id: Uuid, items: &[StandardItemSpec]) -> DbResult<u64> {
    ex.exec_sql(
        format!("DELETE FROM {TABLE_ITEM} WHERE standard_id = ?::uuid"),
        vec![Value::from(&standard_id)],
    )
    .await?;
    let mut inserted = 0u64;
    let sql = insert_item_sql();
    for (index, item) in items.iter().enumerate() {
        let amount = item.amount();
        inserted += ex
            .exec_sql(
                sql.clone(),
                vec![
                    Value::from(&Uuid::now_v7()),
                    Value::from(&standard_id),
                    Value::I32(index as i32 + dto::FIRST_LINE_NO),
                    Value::from(&item.component_material_id),
                    Value::Text(item.std_qty.to_string()),
                    Value::Text(item.std_price.to_string()),
                    Value::Text(amount.to_string()),
                ],
            )
            .await?;
    }
    Ok(inserted)
}

/// 置状态（生效 / 失效），备注一并写回（失效原因留痕用）。返回受影响行数。
pub async fn set_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    status: i16,
    remark: Option<&str>,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    ex.exec_sql(
        set_status_sql(),
        vec![
            Value::I16(status),
            Value::Text(remark.unwrap_or_default().to_string()),
            Value::Text(operator.map(|id| id.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

/// 软删。返回受影响行数（`0` → 不存在 / 已删）。
pub async fn soft_delete(ex: &impl SqlExecutor, id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    ex.exec_sql(
        soft_delete_sql(),
        vec![
            Value::Text(operator.map(|id| id.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

// ==================== 单测（不连库：只验 SQL 形状 / 占位符 / 版本号） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr as _;

    fn spec() -> StandardSpec {
        StandardSpec {
            material_id: Uuid::now_v7(),
            effective_from: NaiveDate::from_ymd_opt(2026, 11, 1).expect("日期"),
            effective_to: Some(NaiveDate::from_ymd_opt(2026, 11, 30).expect("日期")),
            std_labor_hours: Decimal::from_str("2.5").expect("数值"),
            std_labor_rate: Decimal::from_str("30").expect("数值"),
            std_overhead_cost: Decimal::from_str("12").expect("数值"),
            status: dto::STATUS_ACTIVE,
            remark: Some("M5-08".to_string()),
            items: vec![StandardItemSpec {
                component_material_id: Uuid::now_v7(),
                std_qty: Decimal::from_str("3").expect("数值"),
                std_price: Decimal::from_str("5.5").expect("数值"),
            }],
        }
    }

    /// 列清单必须与 `from_row` 的取值列一一对上（漏一列 = 运行时 `get_typed` 炸）。
    #[test]
    fn select_sql_prefixes_every_column_and_joins_material() {
        let sql = select_sql();
        for column in COLUMNS.split(',') {
            let column = column.trim();
            assert!(sql.contains(&format!("t.{column}")), "缺列 {column}: {sql}");
        }
        assert!(sql.contains("FROM biz_cost_standard t"), "{sql}");
        assert!(
            sql.contains("LEFT JOIN biz_material m ON m.id = t.material_id"),
            "必须实时 join 物料取编码/名称（约定 13）：{sql}"
        );
        for alias in [
            "m.material_code AS material_code",
            "m.material_name AS material_name",
            "m.material_type AS material_type",
            "m.spec          AS material_spec",
            "AS items_count",
        ] {
            assert!(sql.contains(alias), "缺派生列 {alias}: {sql}");
        }
    }

    /// 插入 / 更新 / 明细插入的 `?` 个数必须与参数向量等长（错位会静默写错列）。
    #[test]
    fn bind_counts_match_params() {
        let spec = spec();
        let amounts = spec.amounts();
        let id = Uuid::now_v7();
        let insert = insert_head_sql();
        assert_eq!(
            insert.matches('?').count(),
            insert_head_params(id, 1, &spec, &amounts, None).len(),
            "建卡头的占位符与参数必须等长"
        );
        let update = update_head_sql();
        assert_eq!(
            update.matches('?').count(),
            update_head_params(id, &spec, &amounts, None).len(),
            "改卡头的占位符与参数必须等长"
        );
        assert_eq!(soft_delete_sql().matches('?').count(), 2, "软删 = 操作人 + id");
        assert_eq!(
            set_status_sql().matches('?').count(),
            4,
            "置状态 = 状态 + 备注 + 操作人 + id"
        );
        assert_eq!(insert_item_sql().matches('?').count(), 7, "明细 = 7 列参数");
    }

    /// 可空列一律走 `NULLIF(?::text,'')`：空串不能变成 `''::date` / `''::uuid`（会 500）。
    #[test]
    fn nullable_columns_use_nullif_and_numeric_casts() {
        let insert = insert_head_sql();
        assert!(insert.contains("NULLIF(?::text,'')::text::date"), "{insert}");
        assert!(insert.contains("NULLIF(?::text,'')::uuid"), "{insert}");
        assert!(insert.contains("?::text::numeric"), "{insert}");
        let update = update_head_sql();
        assert!(update.contains("NULLIF(?::text,'')::text::date"), "{update}");
        assert!(update.contains("?::text::numeric"), "{update}");
    }

    /// 分页锚点必须还是 unscoped（否则 `paginate_unscoped` 会直接报错）。
    #[test]
    fn target_is_unscoped_and_ordered_stably() {
        assert_eq!(TARGET.table, TABLE_HEAD);
        assert!(!TARGET.is_scoped(), "标准成本卡是全公司口径配置，不参与数据权限");
        assert!(ORDER.contains("t.status = 1"), "启用在前：{ORDER}");
        assert!(ORDER.contains("t.effective_from DESC"), "最新一版在上：{ORDER}");
        assert!(ORDER.contains("t.id ASC"), "末位按 id 保证翻页稳定：{ORDER}");
    }

    /// 版本号 = 同物料存活版本 max + 1（空表从 1 起，失效版本也占号）。
    #[test]
    fn next_version_no_is_max_plus_one() {
        assert_eq!(next_version_no(&[]), 1);
        let mut rows = Vec::new();
        for (version_no, status) in [
            (1, dto::STATUS_ACTIVE),
            (2, dto::STATUS_INACTIVE),
            (7, dto::STATUS_ACTIVE),
        ] {
            rows.push(StandardRow {
                id: Uuid::now_v7(),
                material_id: Uuid::now_v7(),
                version_no,
                effective_from: NaiveDate::from_ymd_opt(2026, 11, 1).expect("日期"),
                effective_to: None,
                std_material_cost: Decimal::ZERO,
                std_labor_cost: Decimal::ZERO,
                std_overhead_cost: Decimal::ZERO,
                std_total_cost: Decimal::ZERO,
                std_labor_hours: Decimal::ZERO,
                std_labor_rate: Decimal::ZERO,
                status,
                remark: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                material_code: None,
                material_name: None,
                material_type: None,
                material_spec: None,
                items_count: 0,
            });
        }
        assert_eq!(next_version_no(&rows), 8, "失效版本也占号，避免号码回收造成两版同号");
    }

    /// 期间矩阵 SQL：占位符个数与 push 顺序一一对应（错位只出错数、不报错）。
    #[test]
    fn period_sheet_sql_placeholder_counts() {
        let plain = period_sheet_sql(false);
        assert_eq!(
            plain.matches('?').count(),
            4,
            "EXISTS 两个日期 + WHERE 两个日期：{plain}"
        );
        assert!(plain.contains("AS switch_in_period"), "{plain}");
        assert!(plain.contains("CASE WHEN EXISTS"), "{plain}");
        assert!(
            plain.contains("FROM biz_cost_standard t LEFT JOIN biz_material m"),
            "外层表与 join 必须完整：{plain}"
        );
        assert!(
            plain.contains("x.effective_from <= ?::text::date"),
            "切换提示按「期间末仍在生效」判：{plain}"
        );
        let keyword = period_sheet_sql(true);
        assert_eq!(keyword.matches('?').count(), 6, "关键字多两个 LIKE：{keyword}");
        assert!(keyword.contains("m.material_code, '') LIKE ?"), "{keyword}");
    }

    /// 物料标称：编码 + 名称；物料缺失时回落 id（不让界面上出现空串）。
    #[test]
    fn material_label_falls_back_to_id() {
        let mut row = StandardRow {
            id: Uuid::now_v7(),
            material_id: Uuid::from_u128(42),
            version_no: 1,
            effective_from: NaiveDate::from_ymd_opt(2026, 11, 1).expect("日期"),
            effective_to: None,
            std_material_cost: Decimal::ZERO,
            std_labor_cost: Decimal::ZERO,
            std_overhead_cost: Decimal::ZERO,
            std_total_cost: Decimal::ZERO,
            std_labor_hours: Decimal::ZERO,
            std_labor_rate: Decimal::ZERO,
            status: dto::STATUS_ACTIVE,
            remark: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            material_code: Some("FG-001".to_string()),
            material_name: Some("成品甲".to_string()),
            material_type: Some("FINISHED".to_string()),
            material_spec: None,
            items_count: 2,
        };
        assert_eq!(row.material_label(), "FG-001（成品甲）");
        row.material_name = None;
        assert_eq!(row.material_label(), "FG-001");
        row.material_code = None;
        assert_eq!(row.material_label(), Uuid::from_u128(42).to_string());
    }
}

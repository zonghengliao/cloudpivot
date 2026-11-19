//! 标准成本 × 实际成本 **逐物料对比** 的取数层（成本核算 · M5-11）。
//!
//! 一句话：**一条 SQL 把「标准侧」和「实际侧」按物料拼起来**，分页交给
//! `scope::paginate_unscoped`，合计交给同一模板的聚合查询（口径不二写）。
//!
//! # 两个 CTE 各管一侧
//!
//! | CTE | 来源 | 口径（逐条写死在 SQL 里） |
//! |---|---|---|
//! | `act` | `biz_cost_period_item` | 只取**当期活跃批次**的 `period_id`；按 `product_id` 聚合完工数量 / 三要素完工成本 / 账面完工成本 / 本期投入 / 期末在制 |
//! | `std` | `biz_cost_standard` | `deleted_at IS NULL` + `status = 1` + `publish_state = 'PUBLISHED'` + **覆盖期间末**（M5-10 的取数闸，本卡不改） |
//! | `draft` | `biz_cost_standard` | 按物料数「未发布版本」个数（缺标准时用来分辨「没建卡」还是「没发布」） |
//!
//! 两侧 `FULL OUTER JOIN`：只有实际没标准、只有标准没实际都要出现在报表里（它们的处置动作完全不同，
//! 用 `state` 区分，见 `cost_standard_compare_dto`）。
//!
//! # 为什么「标准侧至多一行」
//!
//! `status = 1` 且窗口覆盖某一天的行至多一条，由库层 `ex_biz_cost_std_active_window`（V0463 的
//! EXCLUDE）保证 —— 与 `cost_standard_model::find_covering_in` 的「仍取 first()」同一口径。
//! 这里**不用** `DISTINCT ON`：它会强制 `ORDER BY`，而 `scope::paginate_*` 明确禁止
//! `base_sql` 里出现 `ORDER BY`（排序必须作为参数单独传入）。绕过约束直插的行由 EXCLUDE 拦住。
//!
//! # 为什么金额算在 SQL 里也在 Rust 里
//!
//! - 逐行金额：`act_*` 是 SQL 逐行 `round(完工数量 × 单位成本, 4)` 聚合的**原始和**（与
//!   `cost_calc_item` 的 `finished_by_formula` 同源）；单位实际成本、标准应发生额、差异额
//!   由 `cost_standard_compare_dto::calc_of` 算（**唯一算数入口**，纯函数可单测）。
//! - 合计：为了「整表合计 = 全集（不是当前页）」必须在 SQL 里再聚合一次；它只做
//!   `round(单位标准 × 完工数量, 4)` 逐行相加，与 Rust 的 `calc_of` 逐字同口径 —— 真库用例
//!   [`totals_match_row_calcs`] 对拍两者，防漂移。
//!
//! # 数据权限
//!
//! unscoped + 权限码（`cost-method:list`）：与 `cost-standard` / `cost-period` 单头同口径 ——
//! 差异报表是**全公司财务口径**，按部门裁剪会让财务看不到别的车间的差异，正好把差异藏起来。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_standard_compare_dto::{
    ActualSide, CompareCore, StandardSide, StateFilter, TotalsCalc,
};
use cloudpivot_kernel::business::shared::scope;

/// 分页锚点（**SCOPE-EXEMPT**：全公司财务口径报表，不是某部门的业务单据）。
pub const TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_cost_period_item");

/// 稳定排序：**可比且差异大的在前** → 不可比的按物料编码 → id（同秒插入也稳定）。
///
/// 报表的主用途是「找出哪个料偏得最多」，所以第一键是「差异额是否为空」（可比行在前），
/// 第二键是差异额绝对值倒序；不可比的行（缺标准 / 缺实际）排在后面，按编码有序。
pub const ORDER: &str = "ORDER BY (v.variance_amount IS NULL) ASC, \
                          ABS(v.variance_amount) DESC NULLS LAST, \
                          COALESCE(v.material_code, '') ASC, v.material_id ASC";

/// 「可比」的 SQL 判据 —— 与 `CompareCore::state` 的 `STATE_BOTH` 分支逐条对应
/// （结构判定，不含金额）。
const COMPARABLE: &str = "w.standard_id IS NOT NULL AND w.actual_material_id IS NOT NULL AND w.completed_qty > 0";

// ==================== 筛选 ====================

/// 报表筛选（handler 归一化后传进来）。
#[derive(Debug, Clone)]
pub struct CompareFilter {
    /// 期间首末（标准侧按**末日**取版本）。
    pub period: (NaiveDate, NaiveDate),
    /// 物料编码 / 名称关键字。
    pub keyword: Option<String>,
    /// 状态筛选。
    pub state: StateFilter,
    /// 当前页（1 起）。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 关键字 → LIKE 模式（与 `cost_standard_model::like_pattern` / `cost_alloc_rule_model` 同口径）。
fn like_pattern(keyword: &Option<String>) -> Option<String> {
    keyword
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| format!("%{}%", text.replace('%', "\\%").replace('_', "\\_")))
}

// ==================== SQL 模板 ====================

/// 单条 SELECT 的公共正文（两个 CTE + 拼接）。
///
/// `has_batch = false` 表示当期没有活跃批次：`act` 走 `WHERE FALSE`（空集），
/// 这样报表仍然出「只有标准侧」的行（全部 `NO_ACTUAL`），不需要另一套 SQL。
///
/// **占位符顺序 = 文本顺序**：`[batch]` → `act` 之前的批 id；`std` 的 EXISTS（期末、期初）；
/// `std` 的 WHERE（期末、期末）；最后可选的 2 个 LIKE。改 SQL 必须同步改 [`params`]。
pub fn base_sql(has_batch: bool, has_keyword: bool) -> String {
    let act_where = if has_batch { "i.period_id = ?::uuid" } else { "FALSE" };
    let mut sql = format!(
        "WITH act AS ( \
             SELECT i.product_id AS material_id, \
                    count(*)::bigint AS object_count, \
                    COALESCE(sum(i.completed_qty), 0) AS completed_qty, \
                    COALESCE(sum(i.finished_cost), 0) AS book_amount, \
                    COALESCE(sum(round(i.completed_qty * i.unit_material, 4)), 0) AS act_material, \
                    COALESCE(sum(round(i.completed_qty * i.unit_labor, 4)), 0) AS act_labor, \
                    COALESCE(sum(round(i.completed_qty * i.unit_overhead, 4)), 0) AS act_overhead, \
                    COALESCE(sum(i.mat_cur + i.labor_cur + i.oh_cur), 0) AS period_input, \
                    COALESCE(sum(i.wip_end_cost), 0) AS wip_end_cost \
               FROM biz_cost_period_item i \
              WHERE {act_where} \
              GROUP BY i.product_id \
         ), std AS ( \
             SELECT t.id AS standard_id, t.material_id, t.version_no, t.effective_from, t.effective_to, \
                    t.std_material_cost, t.std_labor_cost, t.std_overhead_cost, t.std_total_cost, \
                    t.std_labor_hours, t.std_labor_rate, t.published_at, \
                    CASE WHEN EXISTS ( \
                             SELECT 1 FROM biz_cost_standard x \
                              WHERE x.deleted_at IS NULL AND x.status = 1 AND x.publish_state = 'PUBLISHED' \
                                AND x.material_id = t.material_id AND x.id <> t.id \
                                AND x.effective_from <= ?::text::date \
                                AND (x.effective_to IS NULL OR x.effective_to >= ?::text::date) \
                         ) THEN TRUE ELSE FALSE END AS switch_in_period \
               FROM biz_cost_standard t \
              WHERE t.deleted_at IS NULL AND t.status = 1 AND t.publish_state = 'PUBLISHED' \
                AND t.effective_from <= ?::text::date \
                AND (t.effective_to IS NULL OR t.effective_to >= ?::text::date) \
         ), draft AS ( \
             SELECT d.material_id, count(*)::bigint AS unpublished_versions \
               FROM biz_cost_standard d \
              WHERE d.deleted_at IS NULL AND d.publish_state <> 'PUBLISHED' \
              GROUP BY d.material_id \
         ) \
         SELECT COALESCE(a.material_id, s.material_id) AS material_id, \
                a.material_id AS actual_material_id, \
                m.material_code AS material_code, \
                m.material_name AS material_name, \
                m.spec AS material_spec, \
                COALESCE(dr.unpublished_versions, 0) AS unpublished_versions, \
                a.object_count, a.completed_qty, a.book_amount, \
                a.act_material, a.act_labor, a.act_overhead, a.period_input, a.wip_end_cost, \
                s.standard_id, s.version_no, s.effective_from, s.effective_to, \
                s.std_material_cost, s.std_labor_cost, s.std_overhead_cost, s.std_total_cost, \
                s.std_labor_hours, s.std_labor_rate, s.published_at, \
                COALESCE(s.switch_in_period, FALSE) AS switch_in_period, \
                CASE WHEN a.material_id IS NULL OR s.standard_id IS NULL OR a.completed_qty = 0 THEN NULL \
                     ELSE round(a.act_material + a.act_labor + a.act_overhead \
                                - round(s.std_total_cost * a.completed_qty, 4), 4) \
                END AS variance_amount \
           FROM act a \
           FULL OUTER JOIN std s ON s.material_id = a.material_id \
           LEFT JOIN biz_material m ON m.id = COALESCE(a.material_id, s.material_id) AND m.deleted_at IS NULL \
           LEFT JOIN draft dr ON dr.material_id = COALESCE(a.material_id, s.material_id)"
    );
    if has_keyword {
        sql.push_str(" WHERE (COALESCE(m.material_code, '') LIKE ? OR COALESCE(m.material_name, '') LIKE ?)");
    }
    sql
}

/// 加状态筛选的外层（`v` 别名固定：排序常量 [`ORDER`] 按它写）。
///
/// 四档与 [`StateFilter`] 一一对应，判据与 `CompareCore::state` 同源：
/// - `comparable`：两侧都在且完工数量 > 0，**且差异额不为 0**（差异为 0 的行对报表没信息量）；
/// - `no_standard`：`standard_id IS NULL`；
/// - `no_actual`：有标准但没实际（无计算单或完工数量 0）。
///
/// 每档都带**恰好一个** `{FILTER}` 注入位，且排在业务条件之前：`scope::paginate_*` 把数据权限片段
/// **原位替换**在这个位置（`RowFilter` 的参数恒前置）。不写注入位时框架会把它追加到 SQL 末尾，
/// 而本 SQL 的末尾是 `LEFT JOIN … ON …` / 子查询收尾 —— 追加过去就是 `… ) v AND 1 = 1`，语法错。
pub fn filtered_sql(has_batch: bool, has_keyword: bool, filter: StateFilter) -> String {
    let base = base_sql(has_batch, has_keyword);
    let marker = scope::FILTER_MARKER;
    match filter {
        StateFilter::All => format!("SELECT * FROM ({base}) v WHERE 1 = 1{marker}"),
        StateFilter::Comparable => format!(
            "SELECT * FROM ({base}) v WHERE 1 = 1{marker} AND v.standard_id IS NOT NULL \
               AND v.actual_material_id IS NOT NULL AND v.completed_qty > 0 \
               AND v.variance_amount <> 0"
        ),
        StateFilter::NoStandard => {
            format!("SELECT * FROM ({base}) v WHERE 1 = 1{marker} AND v.standard_id IS NULL")
        }
        StateFilter::NoActual => format!(
            "SELECT * FROM ({base}) v WHERE 1 = 1{marker} AND v.standard_id IS NOT NULL \
               AND (v.actual_material_id IS NULL OR v.completed_qty = 0)"
        ),
    }
}

/// 合计 SQL（整表，**不随分页变**）：行的结构计数 + 可比的三要素金额 + 缺标准的实际金额。
///
/// 只做「逐行 round 后相加」（`round(单位标准 × 完工数量, 4)`），与
/// `cost_standard_compare_dto::calc_of` 逐字同口径；不做任何二次推导（差异额 / 差异率由 Rust 算）。
pub fn totals_sql(has_batch: bool, has_keyword: bool, filter: StateFilter) -> String {
    // 合计走 `db().query_sql`（不过 `scope::paginate_*`），所以要把注入位摘掉：
    // 标记留着就是 `{FILTER}` 字面量 ⇒ 语法错。摘掉后 `WHERE 1 = 1{FILTER} AND …` 自然衔接。
    let filtered = filtered_sql(has_batch, has_keyword, filter).replace(scope::FILTER_MARKER, "");
    format!(
        "SELECT count(*)::bigint AS rows, \
                count(*) FILTER (WHERE {COMPARABLE})::bigint AS comparable_rows, \
                count(*) FILTER (WHERE w.standard_id IS NULL)::bigint AS no_standard_rows, \
                count(*) FILTER (WHERE w.standard_id IS NOT NULL \
                                  AND (w.actual_material_id IS NULL OR w.completed_qty = 0))::bigint \
                    AS no_actual_rows, \
                COALESCE(sum(w.completed_qty) FILTER (WHERE {COMPARABLE}), 0) AS completed_qty, \
                COALESCE(sum(round(w.std_material_cost * w.completed_qty, 4)) \
                         FILTER (WHERE {COMPARABLE}), 0) AS std_material, \
                COALESCE(sum(round(w.std_labor_cost * w.completed_qty, 4)) \
                         FILTER (WHERE {COMPARABLE}), 0) AS std_labor, \
                COALESCE(sum(round(w.std_overhead_cost * w.completed_qty, 4)) \
                         FILTER (WHERE {COMPARABLE}), 0) AS std_overhead, \
                COALESCE(sum(w.act_material) FILTER (WHERE {COMPARABLE}), 0) AS act_material, \
                COALESCE(sum(w.act_labor) FILTER (WHERE {COMPARABLE}), 0) AS act_labor, \
                COALESCE(sum(w.act_overhead) FILTER (WHERE {COMPARABLE}), 0) AS act_overhead, \
                COALESCE(sum(w.book_amount) FILTER (WHERE {COMPARABLE}), 0) AS book_amount, \
                COALESCE(sum(w.act_material + w.act_labor + w.act_overhead) \
                         FILTER (WHERE w.standard_id IS NULL), 0) AS no_standard_amount \
           FROM ({filtered}) w"
    )
}

/// 绑定参数（顺序见 [`base_sql`] 的文档）。
pub fn params(filter: &CompareFilter, batch_id: Option<Uuid>) -> Vec<Value> {
    let (start, end) = filter.period;
    let mut out: Vec<Value> = Vec::with_capacity(7);
    if let Some(batch_id) = batch_id {
        out.push(Value::from(&batch_id));
    }
    // std 的 EXISTS：覆盖「期间内**别的**版本也生效过」→ 期末 / 期初
    out.push(Value::Text(end.to_string()));
    out.push(Value::Text(start.to_string()));
    // std 的 WHERE：覆盖期间末 → 期末 / 期末
    out.push(Value::Text(end.to_string()));
    out.push(Value::Text(end.to_string()));
    if let Some(pattern) = like_pattern(&filter.keyword) {
        out.push(Value::from(pattern.clone()));
        out.push(Value::from(pattern));
    }
    out
}

/// 是否有批次（决定占位符与 `act` 分支，两处必须同源）。
fn has_batch(batch_id: Option<Uuid>) -> bool {
    batch_id.is_some()
}

// ==================== 读 ====================

/// 报表数据页（`total` = 筛选后命中总数）。
pub async fn page(filter: &CompareFilter, batch_id: Option<Uuid>) -> DbResult<(Vec<CompareCore>, i64)> {
    let sql = filtered_sql(
        has_batch(batch_id),
        like_pattern(&filter.keyword).is_some(),
        filter.state,
    );
    let bound = params(filter, batch_id);
    let result = scope::paginate_unscoped(TARGET, &sql, bound, Some(ORDER), filter.page, filter.page_size).await?;
    let rows = result
        .list
        .iter()
        .map(core_of)
        .collect::<DbResult<Vec<CompareCore>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 整表合计（筛选后的全集）。
pub async fn totals(filter: &CompareFilter, batch_id: Option<Uuid>) -> DbResult<TotalsCalc> {
    let sql = totals_sql(
        has_batch(batch_id),
        like_pattern(&filter.keyword).is_some(),
        filter.state,
    );
    let rows = db().query_sql(sql, params(filter, batch_id)).await?;
    let row = rows
        .first()
        .ok_or_else(|| DbError::InvalidValue("合计查询必须返回一行".to_string()))?;
    Ok(TotalsCalc {
        rows: row.get_typed("rows")?,
        comparable_rows: row.get_typed("comparable_rows")?,
        no_standard_rows: row.get_typed("no_standard_rows")?,
        no_actual_rows: row.get_typed("no_actual_rows")?,
        completed_qty: row.get_typed("completed_qty")?,
        std_amount: [
            row.get_typed("std_material")?,
            row.get_typed("std_labor")?,
            row.get_typed("std_overhead")?,
        ],
        act_amount: [
            row.get_typed("act_material")?,
            row.get_typed("act_labor")?,
            row.get_typed("act_overhead")?,
        ],
        book_amount: row.get_typed("book_amount")?,
        no_standard_amount: row.get_typed("no_standard_amount")?,
    })
}

/// 查询行 → [`CompareCore`]（两侧各自的 `Option` 由列是否为空决定）。
fn core_of(row: &Row) -> DbResult<CompareCore> {
    let standard_id: Option<Uuid> = row.get_typed("standard_id")?;
    let actual_material_id: Option<Uuid> = row.get_typed("actual_material_id")?;
    let standard = match standard_id {
        None => None,
        Some(standard_id) => Some(StandardSide {
            standard_id,
            version_no: row.get_typed("version_no")?,
            effective_from: row.get_typed("effective_from")?,
            effective_to: row.get_typed("effective_to")?,
            std_material_cost: row.get_typed("std_material_cost")?,
            std_labor_cost: row.get_typed("std_labor_cost")?,
            std_overhead_cost: row.get_typed("std_overhead_cost")?,
            std_total_cost: row.get_typed("std_total_cost")?,
            std_labor_hours: row.get_typed("std_labor_hours")?,
            std_labor_rate: row.get_typed("std_labor_rate")?,
            published_at: row.get_typed("published_at")?,
            switch_in_period: row.get_typed("switch_in_period")?,
        }),
    };
    let actual = match actual_material_id {
        None => None,
        Some(_) => Some(ActualSide {
            object_count: row.get_typed::<Option<i64>>("object_count")?.unwrap_or(0),
            completed_qty: row
                .get_typed::<Option<Decimal>>("completed_qty")?
                .unwrap_or(Decimal::ZERO),
            book_amount: row
                .get_typed::<Option<Decimal>>("book_amount")?
                .unwrap_or(Decimal::ZERO),
            amount_material: row
                .get_typed::<Option<Decimal>>("act_material")?
                .unwrap_or(Decimal::ZERO),
            amount_labor: row.get_typed::<Option<Decimal>>("act_labor")?.unwrap_or(Decimal::ZERO),
            amount_overhead: row
                .get_typed::<Option<Decimal>>("act_overhead")?
                .unwrap_or(Decimal::ZERO),
            period_input: row
                .get_typed::<Option<Decimal>>("period_input")?
                .unwrap_or(Decimal::ZERO),
            wip_end_cost: row
                .get_typed::<Option<Decimal>>("wip_end_cost")?
                .unwrap_or(Decimal::ZERO),
        }),
    };
    Ok(CompareCore {
        material_id: row.get_typed("material_id")?,
        material_code: row.get_typed("material_code")?,
        material_name: row.get_typed("material_name")?,
        material_spec: row.get_typed("material_spec")?,
        unpublished_versions: row.get_typed::<Option<i64>>("unpublished_versions")?.unwrap_or(0),
        standard,
        actual,
    })
}

// ==================== 单测（SQL 形状，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(state: StateFilter, keyword: Option<&str>) -> CompareFilter {
        CompareFilter {
            period: (
                NaiveDate::from_ymd_opt(2026, 11, 1).expect("期初"),
                NaiveDate::from_ymd_opt(2026, 11, 30).expect("期末"),
            ),
            keyword: keyword.map(str::to_string),
            state,
            page: 1,
            page_size: 20,
        }
    }

    #[test]
    fn base_sql_keeps_the_publish_gate_and_period_end_basis() {
        let sql = base_sql(true, false);
        // M5-10 的取数闸：只有已发布 + 启用的卡参与对比
        assert!(sql.contains("t.publish_state = 'PUBLISHED'"), "{sql}");
        assert!(sql.contains("t.status = 1"), "{sql}");
        // 期间末生效：effective_from <= 期末 AND (effective_to IS NULL OR >= 期末)
        assert!(sql.contains("t.effective_from <= ?::text::date"), "{sql}");
        assert!(
            sql.contains("(t.effective_to IS NULL OR t.effective_to >= ?::text::date)"),
            "{sql}"
        );
        // 实际侧只读当期批次的计算单
        assert!(sql.contains("WHERE i.period_id = ?::uuid"), "{sql}");
        assert!(sql.contains("FROM biz_cost_period_item i"), "{sql}");
    }

    #[test]
    fn base_sql_without_batch_uses_false_and_keeps_placeholder_count() {
        let sql = base_sql(false, false);
        assert!(sql.contains("WHERE FALSE"), "无批次时 act 必须空集：{sql}");
        assert!(!sql.contains("?::uuid"), "无批次时不该留批 id 占位符：{sql}");
        // 参数个数 = std 的 4 个日期占位符（EXISTS 2 + WHERE 2）
        assert_eq!(sql.matches("?::text::date").count(), 4, "{sql}");
        let bound = params(&filter(StateFilter::All, None), None);
        assert_eq!(bound.len(), 4, "无批次无关键字 ⇒ 4 个参数");
    }

    #[test]
    fn params_follow_textual_placeholder_order() {
        let batch = Uuid::from_u128(0x5100_0000_0000_0000_0000_0000_0000_0001);
        let bound = params(&filter(StateFilter::All, Some("FG")), Some(batch));
        assert_eq!(bound.len(), 7, "批 id + 4 个日期 + 2 个 LIKE");
        assert_eq!(bound[0], Value::from(&batch), "批 id 必须在最前（act 在 CTE 最前）");
        assert_eq!(bound[1], Value::Text("2026-11-30".to_string()), "EXISTS 先期末");
        assert_eq!(bound[2], Value::Text("2026-11-01".to_string()), "EXISTS 后期初");
        assert_eq!(bound[3], Value::Text("2026-11-30".to_string()), "WHERE 先期末");
        assert_eq!(bound[4], Value::Text("2026-11-30".to_string()), "WHERE 后期末");
        assert_eq!(bound[5], Value::from("%FG%".to_string()));
    }

    #[test]
    fn keyword_is_escaped_and_absent_when_blank() {
        let sql = base_sql(true, false);
        assert!(!sql.contains("LIKE"), "无关键字时不该出现 LIKE：{sql}");
        let bound = params(&filter(StateFilter::All, Some("  ")), None);
        assert_eq!(bound.len(), 4, "空白关键字等于没有");

        let wildcard = params(&filter(StateFilter::All, Some("A_B%")), None);
        assert_eq!(
            wildcard[5],
            Value::from("%A\\_B\\%%".to_string()),
            "通配符必须转义，否则用户输入的下划线会变成单字符通配"
        );
    }

    #[test]
    fn state_filters_only_use_structural_predicates() {
        let all = filtered_sql(true, false, StateFilter::All);
        assert!(all.starts_with("SELECT * FROM (WITH act AS ("), "{all}");
        assert!(!all.contains(" WHERE v."), "all 不加业务条件：{all}");

        let comparable = filtered_sql(true, false, StateFilter::Comparable);
        assert!(comparable.contains("v.variance_amount <> 0"), "{comparable}");
        assert!(comparable.contains("v.completed_qty > 0"), "{comparable}");

        let no_standard = filtered_sql(true, false, StateFilter::NoStandard);
        assert!(no_standard.contains("AND v.standard_id IS NULL"), "{no_standard}");

        let no_actual = filtered_sql(true, false, StateFilter::NoActual);
        assert!(
            no_actual.contains("v.actual_material_id IS NULL OR v.completed_qty = 0"),
            "{no_actual}"
        );
    }

    #[test]
    fn every_filter_branch_carries_exactly_one_filter_marker() {
        // `scope::paginate_*` 把权限片段**原位替换**在标记处；没有标记时它会追加到 SQL 末尾，
        // 而本 SQL 末尾是子查询收尾（`) v`）⇒ 追加过去就是 `) v AND 1 = 1`，语法错。
        for state in [
            StateFilter::All,
            StateFilter::Comparable,
            StateFilter::NoStandard,
            StateFilter::NoActual,
        ] {
            let sql = filtered_sql(true, true, state);
            assert_eq!(
                sql.matches(scope::FILTER_MARKER).count(),
                1,
                "{state:?} 必须恰好一个注入位：{sql}"
            );
            // 注入位必须在业务条件之前（片段参数恒前置，架构 §2.6）
            let marker = sql.find(scope::FILTER_MARKER).expect("有标记");
            let first_business = sql.find(" AND v.").unwrap_or(usize::MAX);
            assert!(marker < first_business, "注入位要在业务条件之前：{sql}");
        }
    }

    #[test]
    fn sql_never_smuggles_order_by_or_limit() {
        // `scope::paginate_*` 明令禁止：排序必须作为参数单独传入（架构 §2.3）
        for state in [
            StateFilter::All,
            StateFilter::Comparable,
            StateFilter::NoStandard,
            StateFilter::NoActual,
        ] {
            let sql = filtered_sql(true, true, state);
            let upper = sql.to_uppercase();
            assert!(!upper.contains("ORDER BY"), "base_sql 不得自带排序：{state:?}");
            assert!(
                !upper
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .any(|word| word == "LIMIT"),
                "base_sql 不得自带 LIMIT：{state:?}"
            );
        }
    }

    #[test]
    fn order_presets_put_comparable_rows_first() {
        assert!(ORDER.contains("ORDER BY (v.variance_amount IS NULL) ASC"), "{ORDER}");
        assert!(ORDER.contains("ABS(v.variance_amount) DESC"), "{ORDER}");
        assert!(
            ORDER.contains("v.material_id ASC"),
            "必须有序数兜底，翻页才稳定：{ORDER}"
        );
    }

    #[test]
    fn totals_sql_aggregates_the_same_filtered_set() {
        let sql = totals_sql(true, true, StateFilter::Comparable);
        assert!(
            sql.contains("FROM (SELECT * FROM (WITH act AS ("),
            "合计要基于同一份筛选 SQL：{sql}"
        );
        assert!(
            sql.contains("FILTER (WHERE") || sql.contains("FILTER (WHERE {COMPARABLE})"),
            "{sql}"
        );
        for column in [
            "rows",
            "comparable_rows",
            "no_standard_rows",
            "no_actual_rows",
            "completed_qty",
            "std_material",
            "std_labor",
            "std_overhead",
            "act_material",
            "act_labor",
            "act_overhead",
            "book_amount",
            "no_standard_amount",
        ] {
            assert!(sql.contains(column), "合计缺列 {column}：{sql}");
        }
        // 标准应发生额必须逐行 round 后再相加（与 calc_of 同口径）
        assert!(sql.contains("round(w.std_material_cost * w.completed_qty, 4)"), "{sql}");
    }

    #[test]
    fn comparable_predicate_matches_dto_state_rule() {
        // 结构判据必须与 CompareCore::state 的 BOTH 分支一致
        assert!(COMPARABLE.contains("standard_id IS NOT NULL"));
        assert!(COMPARABLE.contains("actual_material_id IS NOT NULL"));
        assert!(COMPARABLE.contains("completed_qty > 0"));
    }
}

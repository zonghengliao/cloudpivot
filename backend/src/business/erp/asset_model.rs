//! 固定资产数据层（ARCH-6 第五章）：分类 / 卡片 / 变动留痕 / 折旧批次与明细。
//!
//! # 表结构的唯一依据
//!
//! `backend/migrations/business/V0229__erp_asset_tables.up.sql`（分类 + 卡片 + 变动）与
//! `V0230__erp_depreciation_tables.up.sql`（批次 + 明细），列名与约束逐字取自 ARCH-6 §5.2。
//!
//! # 四条铁律（本文件不许违反）
//!
//! 1. **缓存列的唯一写入者**（§5.5.4）：`accumulated_depreciation / net_value /
//!    depreciation_months_done / last_depreciation_period` 只由
//!    [`apply_depreciation_to_asset`] 与 [`rollback_depreciation_on_asset`] 写
//!    （折旧过账 / 红冲）。建卡、转移、处置**一行都不碰**。
//! 2. **主表当前值可覆盖，但每次覆盖必须同事务写 `biz_fixed_asset_change`**（§5.2.3）：
//!    [`insert_change`] 与状态回写总是成对出现，调用方负责同一事务。
//! 3. **状态只能走状态机**：本文件只提供 CAS 回写 [`apply_asset_status`]，
//!    合法边判定与 `biz_status_log` 由 `state_machine::transition` 负责。
//! 4. **金额列一律 `::text` 取出再解析**（驱动不把 `numeric` 映到 `Decimal`）——
//!    与 `budget_model` / `payable` 同一口径，别图省事直接 `get_typed::<Decimal>`。
//!
//! # 为什么资产表不做数据权限锚点
//!
//! `biz_fixed_asset` 有 `use_dept_id`（归属部门），但资产是**资产台账**：
//! 财务要能看全公司净值，部门主管看本部门只是默认视图。本卡按
//! `ScopeTarget::unscoped` 处理（与 13 张 ERP 主数据同款），把部门裁剪留给
//! 前端的筛选条件 —— 若将来要按部门做行级权限，把这里的 `unscoped` 换成
//! `ScopeTarget::new(TABLE, "a", Some("use_dept_id"), None)` 并让 handler 走
//! `scope::paginate(…, &principal)` 即可（`budget_model` 是现成的例子）。

use std::collections::HashMap;

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::asset_dto;
use crate::business::erp::asset_dto::{AssetFilter, CategoryFilter, CategorySpec, CreateAssetSpec, DepBatchFilter};
use crate::business::erp::voucher::{self, SeqSpec};
use crate::business::shared::scope;

/// 资产分类表名。
pub const CATEGORY_TABLE: &str = "biz_asset_category";
/// 资产卡片表名。
pub const TABLE: &str = "biz_fixed_asset";
/// 变动留痕表名。
pub const CHANGE_TABLE: &str = "biz_fixed_asset_change";
/// 折旧批次表名。
pub const DEP_BATCH_TABLE: &str = "biz_depreciation_batch";
/// 折旧明细表名。
pub const DEP_TABLE: &str = "biz_depreciation";

/// 资产编号号段（`FA-yyyyMMdd-000001`，与其它单据同款按天滚动）。
///
/// 设计 §5.2.2 写的是 `FA-2026-0001`（按年），但全库取号通道 `biz_seq` 的
/// `date_pattern` 只有 `yyyyMMdd` 一种口径；按年编号要改共享取号器（影响 4 张单据），
/// 不值得。差异写进证据 README：**编号形态 `FA-yyyyMMdd-NNNNNN`，唯一性口径不变**。
pub const SEQ: SeqSpec<'static> = SeqSpec {
    seq_type: "FA",
    prefix: "FA",
    seq_name: "固定资产编号",
    remark: "固定资产编号：FA-yyyyMMdd-000001，按天（CST）滚动",
};

/// 折旧批次号号段（`DEP-yyyyMMdd-000001`；设计 §5.2.4 写的 `DEP-2026-10` 同理顺延）。
pub const BATCH_SEQ: SeqSpec<'static> = SeqSpec {
    seq_type: "DEP",
    prefix: "DEP",
    seq_name: "折旧批次号",
    remark: "折旧批次号：DEP-yyyyMMdd-000001（期间唯一由 uk_biz_dep_batch_period 保证）",
};

/// 资产分页锚点：不上行级权限（见模块文档），走 `paginate_unscoped`。
pub const ASSET_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(TABLE);
/// 分类分页锚点（全局主数据）。
pub const CATEGORY_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(CATEGORY_TABLE);
/// 折旧批次分页锚点。
pub const BATCH_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(DEP_BATCH_TABLE);

fn parse_amount(text: &str) -> Decimal {
    text.parse::<Decimal>().unwrap_or(Decimal::ZERO)
}

fn parse_filter_uuid(raw: Option<&str>) -> Option<Uuid> {
    raw.map(str::trim)
        .filter(|text| !text.is_empty())
        .and_then(|text| Uuid::parse_str(text).ok())
}

/// 可空文本 → `Value`（空 / `None` → `Text("")`，SQL 里用 `NULLIF(?::text,'')`）。
fn opt_text(value: Option<&str>) -> Value {
    Value::Text(value.map(str::trim).unwrap_or_default().to_string())
}

/// 可空 uuid → `Value`（同上，SQL 用 `NULLIF(?::text,'')::uuid`）。
fn opt_uuid(value: Option<&Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

// ==================== ① 资产分类 ====================

/// 分类行。
#[derive(Debug, Clone)]
pub struct CategoryRow {
    /// 主键。
    pub id: Uuid,
    /// 编码。
    pub category_code: String,
    /// 名称。
    pub category_name: String,
    /// 折旧方法。
    pub depreciation_method: String,
    /// 使用月数。
    pub useful_life_months: i32,
    /// 净残值率。
    pub salvage_rate: Decimal,
    /// 原值科目。
    pub asset_account_id: Uuid,
    /// 累计折旧科目。
    pub accum_account_id: Uuid,
    /// 折旧费用科目。
    pub expense_account_id: Uuid,
    /// 状态。
    pub status: i16,
    /// 排序。
    pub sort_order: i32,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

const CATEGORY_COLUMNS: &str = "c.id, c.category_code, c.category_name, c.depreciation_method, \
     c.useful_life_months, c.salvage_rate::text AS salvage_rate, c.asset_account_id, \
     c.accum_account_id, c.expense_account_id, c.status, c.sort_order, c.remark, c.created_at";

impl CategoryRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            category_code: row.get_typed("category_code")?,
            category_name: row.get_typed("category_name")?,
            depreciation_method: row.get_typed("depreciation_method")?,
            useful_life_months: row.get_typed("useful_life_months")?,
            salvage_rate: parse_amount(&row.get_typed::<String>("salvage_rate")?),
            asset_account_id: row.get_typed("asset_account_id")?,
            accum_account_id: row.get_typed("accum_account_id")?,
            expense_account_id: row.get_typed("expense_account_id")?,
            status: row.get_typed("status")?,
            sort_order: row.get_typed("sort_order")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
        })
    }
}

/// 分类分页（软删不返回）。
pub async fn category_page(q: &CategoryFilter, page: u64, page_size: u64) -> DbResult<(Vec<CategoryRow>, i64)> {
    // SCOPE-EXEMPT: 分类是全局主数据，没有归属列。
    let mut sql = format!("SELECT {CATEGORY_COLUMNS} FROM {CATEGORY_TABLE} c WHERE c.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(keyword) = q.keyword.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        sql.push_str(" AND (c.category_code ILIKE ? OR c.category_name ILIKE ?)");
        let like = format!("%{keyword}%");
        params.push(Value::Text(like.clone()));
        params.push(Value::Text(like));
    }
    if let Some(status) = q.status {
        sql.push_str(" AND c.status = ?::int");
        params.push(Value::from(i32::from(status)));
    }
    let order = "ORDER BY c.sort_order ASC, c.category_code ASC";
    let result = scope::paginate_unscoped(CATEGORY_TARGET, &sql, params, Some(order), page, page_size).await?;
    let rows = result
        .list
        .iter()
        .map(CategoryRow::from_row)
        .collect::<DbResult<Vec<_>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 全部启用分类（建卡下拉用，不分页）。
pub async fn category_active() -> DbResult<Vec<CategoryRow>> {
    // SCOPE-EXEMPT: 同上。
    let rows = db()
        .find_with_params(
            &format!(
                "SELECT {CATEGORY_COLUMNS} FROM {CATEGORY_TABLE} c \
                      WHERE c.deleted_at IS NULL AND c.status = 1 \
                      ORDER BY c.sort_order ASC, c.category_code ASC"
            ),
            vec![],
        )
        .await?;
    rows.iter().map(CategoryRow::from_row).collect()
}

/// 按 id 取分类（同事务版）。
pub async fn category_find_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<CategoryRow>> {
    // SCOPE-EXEMPT: 主键精确取 1 行。
    let rows = ex
        .query_sql(
            format!(
                "SELECT {CATEGORY_COLUMNS} FROM {CATEGORY_TABLE} c \
                  WHERE c.id = ?::uuid AND c.deleted_at IS NULL"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(CategoryRow::from_row).transpose()
}

/// 按 id 取分类。
pub async fn category_find(id: Uuid) -> DbResult<Option<CategoryRow>> {
    category_find_in(db(), id).await
}

/// 插入分类。
pub async fn category_insert(
    ex: &impl SqlExecutor,
    id: Uuid,
    spec: &CategorySpec,
    asset_account_id: Uuid,
    accum_account_id: Uuid,
    expense_account_id: Uuid,
    operator: Option<Uuid>,
) -> DbResult<()> {
    // SCOPE-EXEMPT: 主数据写入，无归属列。
    ex.exec_sql(
        format!(
            "INSERT INTO {CATEGORY_TABLE} \
                (id, category_code, category_name, depreciation_method, useful_life_months, \
                 salvage_rate, asset_account_id, accum_account_id, expense_account_id, \
                 status, sort_order, remark, create_by, update_by) \
             VALUES (?::uuid, ?, ?, 'STRAIGHT_LINE', ?::int, ?::text::numeric, ?::uuid, ?::uuid, ?::uuid, \
                     ?::int, ?::int, NULLIF(?::text,''), NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
        ),
        vec![
            Value::from(&id),
            Value::Text(spec.category_code.clone()),
            Value::Text(spec.category_name.clone()),
            Value::from(spec.useful_life_months),
            Value::Text(spec.salvage_rate.to_string()),
            Value::from(&asset_account_id),
            Value::from(&accum_account_id),
            Value::from(&expense_account_id),
            Value::from(i32::from(spec.status)),
            Value::from(spec.sort_order),
            opt_text(spec.remark.as_deref()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
        ],
    )
    .await?;
    Ok(())
}

/// 更新分类（返回受影响行数）。
pub async fn category_update(
    ex: &impl SqlExecutor,
    id: Uuid,
    spec: &CategorySpec,
    asset_account_id: Uuid,
    accum_account_id: Uuid,
    expense_account_id: Uuid,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主数据写入。
    ex.exec_sql(
        format!(
            "UPDATE {CATEGORY_TABLE} SET category_name = ?, useful_life_months = ?::int, \
                    salvage_rate = ?::text::numeric, asset_account_id = ?::uuid, \
                    accum_account_id = ?::uuid, expense_account_id = ?::uuid, \
                    status = ?::int, sort_order = ?::int, remark = NULLIF(?::text,''), \
                    updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(spec.category_name.clone()),
            Value::from(spec.useful_life_months),
            Value::Text(spec.salvage_rate.to_string()),
            Value::from(&asset_account_id),
            Value::from(&accum_account_id),
            Value::from(&expense_account_id),
            Value::from(i32::from(spec.status)),
            Value::from(spec.sort_order),
            opt_text(spec.remark.as_deref()),
            Value::from(Utc::now()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            Value::from(&id),
        ],
    )
    .await
}

/// 软删分类。
pub async fn category_soft_delete(ex: &impl SqlExecutor, id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主数据写入。
    ex.exec_sql(
        format!(
            "UPDATE {CATEGORY_TABLE} SET deleted_at = ?, updated_at = ?, \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![
            Value::from(Utc::now()),
            Value::from(Utc::now()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            Value::from(&id),
        ],
    )
    .await
}

/// 该分类下还有多少张未删除的卡片（删分类前置校验）。
pub async fn category_asset_count(ex: &impl SqlExecutor, category_id: Uuid) -> DbResult<i64> {
    // SCOPE-EXEMPT: 计数。
    ex.scalar_sql(
        format!("SELECT count(*) FROM {TABLE} WHERE category_id = ?::uuid AND deleted_at IS NULL"),
        vec![Value::from(&category_id)],
    )
    .await
}

// ==================== ② 资产卡片 ====================

/// 卡片行。
#[derive(Debug, Clone)]
pub struct AssetRow {
    /// 主键。
    pub id: Uuid,
    /// 资产编号。
    pub asset_no: String,
    /// 名称。
    pub asset_name: String,
    /// 分类 id。
    pub category_id: Uuid,
    /// 分类编码（快照）。
    pub category_code: String,
    /// 规格。
    pub spec: Option<String>,
    /// 使用部门。
    pub use_dept_id: Uuid,
    /// 使用部门名（快照）。
    pub use_dept_name: String,
    /// 使用人。
    pub custodian_id: Option<Uuid>,
    /// 使用人姓名。
    pub custodian_name: Option<String>,
    /// 存放地点。
    pub location: Option<String>,
    /// 原值。
    pub original_value: Decimal,
    /// 可抵扣进项税。
    pub tax_amount: Decimal,
    /// 预计净残值。
    pub residual_value: Decimal,
    /// 可折旧总额。
    pub depreciable_base: Decimal,
    /// 折旧方法。
    pub depreciation_method: String,
    /// 使用月数。
    pub useful_life_months: i32,
    /// 净残值率。
    pub salvage_rate: Decimal,
    /// 月折旧额。
    pub monthly_depreciation: Decimal,
    /// 开始计提日。
    pub start_depreciation_date: NaiveDate,
    /// 累计折旧。
    pub accumulated_depreciation: Decimal,
    /// 净值。
    pub net_value: Decimal,
    /// 已提月数。
    pub depreciation_months_done: i32,
    /// 最后计提期间。
    pub last_depreciation_period: Option<String>,
    /// 购入日期。
    pub purchase_date: NaiveDate,
    /// 来源类型。
    pub source_type: String,
    /// 来源单 id。
    pub source_id: Option<Uuid>,
    /// 来源单号。
    pub source_no: Option<String>,
    /// 购入口径。
    pub buy_mode: Option<String>,
    /// 供应商。
    pub supplier_id: Option<Uuid>,
    /// 供应商名。
    pub supplier_name: Option<String>,
    /// 购入凭证。
    pub voucher_id: Option<Uuid>,
    /// 状态。
    pub status: String,
    /// 处置类型。
    pub disposal_type: Option<String>,
    /// 处置日期。
    pub disposal_date: Option<NaiveDate>,
    /// 处置凭证。
    pub disposal_voucher_id: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

const ASSET_COLUMNS: &str = "a.id, a.asset_no, a.asset_name, a.category_id, a.category_code, a.spec, \
     a.use_dept_id, a.use_dept_name, a.custodian_id, a.custodian_name, a.location, \
     a.original_value::text AS original_value, a.tax_amount::text AS tax_amount, \
     a.residual_value::text AS residual_value, a.depreciable_base::text AS depreciable_base, \
     a.depreciation_method, a.useful_life_months, a.salvage_rate::text AS salvage_rate, \
     a.monthly_depreciation::text AS monthly_depreciation, a.start_depreciation_date, \
     a.accumulated_depreciation::text AS accumulated_depreciation, \
     a.net_value::text AS net_value, a.depreciation_months_done, a.last_depreciation_period, \
     a.purchase_date, a.source_type, a.source_id, a.source_no, a.buy_mode, a.supplier_id, \
     a.supplier_name, a.voucher_id, a.status, a.disposal_type, a.disposal_date, \
     a.disposal_voucher_id, a.remark, a.created_at";

impl AssetRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            asset_no: row.get_typed("asset_no")?,
            asset_name: row.get_typed("asset_name")?,
            category_id: row.get_typed("category_id")?,
            category_code: row.get_typed("category_code")?,
            spec: row.get_typed("spec")?,
            use_dept_id: row.get_typed("use_dept_id")?,
            use_dept_name: row.get_typed("use_dept_name")?,
            custodian_id: row.get_typed("custodian_id")?,
            custodian_name: row.get_typed("custodian_name")?,
            location: row.get_typed("location")?,
            original_value: parse_amount(&row.get_typed::<String>("original_value")?),
            tax_amount: parse_amount(&row.get_typed::<String>("tax_amount")?),
            residual_value: parse_amount(&row.get_typed::<String>("residual_value")?),
            depreciable_base: parse_amount(&row.get_typed::<String>("depreciable_base")?),
            depreciation_method: row.get_typed("depreciation_method")?,
            useful_life_months: row.get_typed("useful_life_months")?,
            salvage_rate: parse_amount(&row.get_typed::<String>("salvage_rate")?),
            monthly_depreciation: parse_amount(&row.get_typed::<String>("monthly_depreciation")?),
            start_depreciation_date: row.get_typed("start_depreciation_date")?,
            accumulated_depreciation: parse_amount(&row.get_typed::<String>("accumulated_depreciation")?),
            net_value: parse_amount(&row.get_typed::<String>("net_value")?),
            depreciation_months_done: row.get_typed("depreciation_months_done")?,
            last_depreciation_period: row.get_typed("last_depreciation_period")?,
            purchase_date: row.get_typed("purchase_date")?,
            source_type: row.get_typed("source_type")?,
            source_id: row.get_typed("source_id")?,
            source_no: row.get_typed("source_no")?,
            buy_mode: row.get_typed("buy_mode")?,
            supplier_id: row.get_typed("supplier_id")?,
            supplier_name: row.get_typed("supplier_name")?,
            voucher_id: row.get_typed("voucher_id")?,
            status: row.get_typed("status")?,
            disposal_type: row.get_typed("disposal_type")?,
            disposal_date: row.get_typed("disposal_date")?,
            disposal_voucher_id: row.get_typed("disposal_voucher_id")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
        })
    }
}

/// 资产分页（软删不返回）。
pub async fn asset_page(q: &AssetFilter, page: u64, page_size: u64) -> DbResult<(Vec<AssetRow>, i64)> {
    // SCOPE-EXEMPT: 资产台账不做行级权限（见模块文档），部门筛选是列表条件。
    let mut sql = format!("SELECT {ASSET_COLUMNS} FROM {TABLE} a WHERE a.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(keyword) = q.keyword.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        sql.push_str(" AND (a.asset_no ILIKE ? OR a.asset_name ILIKE ? OR a.spec ILIKE ?)");
        let like = format!("%{keyword}%");
        params.push(Value::Text(like.clone()));
        params.push(Value::Text(like.clone()));
        params.push(Value::Text(like));
    }
    if let Some(category_id) = parse_filter_uuid(q.category_id.as_deref()) {
        sql.push_str(" AND a.category_id = ?::uuid");
        params.push(Value::from(&category_id));
    }
    if let Some(dept_id) = parse_filter_uuid(q.use_dept_id.as_deref()) {
        sql.push_str(" AND a.use_dept_id = ?::uuid");
        params.push(Value::from(&dept_id));
    }
    if let Some(status) = q.status.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        sql.push_str(" AND a.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(source_type) = q.source_type.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        sql.push_str(" AND a.source_type = ?");
        params.push(Value::Text(source_type.to_string()));
    }
    let order = "ORDER BY a.purchase_date DESC, a.asset_no DESC";
    let result = scope::paginate_unscoped(ASSET_TARGET, &sql, params, Some(order), page, page_size).await?;
    let rows = result
        .list
        .iter()
        .map(AssetRow::from_row)
        .collect::<DbResult<Vec<_>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 按 id 取卡片。
pub async fn asset_find(id: Uuid) -> DbResult<Option<AssetRow>> {
    asset_find_in(db(), id).await
}

/// 按 id 取卡片（同事务版）。
pub async fn asset_find_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<AssetRow>> {
    // SCOPE-EXEMPT: 主键精确取 1 行。
    let rows = ex
        .query_sql(
            format!("SELECT {ASSET_COLUMNS} FROM {TABLE} a WHERE a.id = ?::uuid AND a.deleted_at IS NULL"),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(AssetRow::from_row).transpose()
}

/// 按 id 取卡片并**加行锁**（`SELECT … FOR UPDATE`）：处置 / 折旧过账必须先锁卡，
/// 否则「审批通过」与「本月计提」并发时两边都用旧累计数算，累计会翻倍或漏提。
pub async fn asset_lock_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<AssetRow>> {
    // SCOPE-EXEMPT: 主键精确取 1 行 + 行锁。
    let rows = ex
        .query_sql(
            format!(
                "SELECT {ASSET_COLUMNS} FROM {TABLE} a \
                  WHERE a.id = ?::uuid AND a.deleted_at IS NULL FOR UPDATE"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(AssetRow::from_row).transpose()
}

/// 取号（资产编号）。
pub async fn next_asset_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    voucher::next_doc_no(ex, &SEQ, now).await
}

/// 取号（折旧批次号）。
pub async fn next_batch_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    voucher::next_doc_no(ex, &BATCH_SEQ, now).await
}

/// 建卡写入所需的全部派生值（由 handler 算好传进来，model 不重算金额）。
#[derive(Debug, Clone)]
pub struct AssetInsert {
    /// 资产编号。
    pub asset_no: String,
    /// 校验后的建卡参数。
    pub spec: CreateAssetSpec,
    /// 分类编码（快照）。
    pub category_code: String,
    /// 部门名（快照）。
    pub use_dept_name: String,
    /// 使用人姓名（快照）。
    pub custodian_name: Option<String>,
    /// 供应商名（快照）。
    pub supplier_name: Option<String>,
    /// 预计净残值。
    pub residual_value: Decimal,
    /// 可折旧总额。
    pub depreciable_base: Decimal,
    /// 月折旧额。
    pub monthly_depreciation: Decimal,
    /// 使用月数（分类快照；建卡时算死，分类改了不影响已建卡）。
    pub useful_life_months: i32,
    /// 净残值率（分类快照）。
    pub salvage_rate: Decimal,
    /// 开始计提日。
    pub start_depreciation_date: NaiveDate,
    /// 操作人。
    pub operator: Option<Uuid>,
}

/// 插入卡片（`net_value` 初始 = 原值，`accumulated = 0`）。
pub async fn asset_insert(ex: &impl SqlExecutor, id: Uuid, input: &AssetInsert) -> DbResult<()> {
    let spec = &input.spec;
    // SCOPE-EXEMPT: 主表写入。
    ex.exec_sql(
        format!(
            "INSERT INTO {TABLE} \
                (id, asset_no, asset_name, category_id, category_code, spec, use_dept_id, \
                 use_dept_name, custodian_id, custodian_name, location, original_value, tax_amount, \
                 residual_value, depreciable_base, depreciation_method, useful_life_months, \
                 salvage_rate, monthly_depreciation, start_depreciation_date, \
                 accumulated_depreciation, net_value, depreciation_months_done, \
                 last_depreciation_period, purchase_date, source_type, source_id, source_no, \
                 buy_mode, supplier_id, supplier_name, status, remark, create_by, update_by) \
             VALUES (?::uuid, ?, ?, ?::uuid, ?, NULLIF(?::text,''), ?::uuid, ?, \
                     NULLIF(?::text,'')::uuid, NULLIF(?::text,''), NULLIF(?::text,''), \
                     ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, 'STRAIGHT_LINE', ?::int, \
                     ?::text::numeric, ?::text::numeric, ?::text::date, 0, ?::text::numeric, 0, NULL, ?::text::date, ?, \
                     NULLIF(?::text,'')::uuid, NULLIF(?::text,''), NULLIF(?::text,''), \
                     NULLIF(?::text,'')::uuid, NULLIF(?::text,''), 'IN_USE', NULLIF(?::text,''), \
                     NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
        ),
        vec![
            Value::from(&id),
            Value::Text(input.asset_no.clone()),
            Value::Text(spec.asset_name.clone()),
            Value::from(&spec.category_id),
            Value::Text(input.category_code.clone()),
            opt_text(spec.spec.as_deref()),
            Value::from(&spec.use_dept_id),
            Value::Text(input.use_dept_name.clone()),
            opt_uuid(spec.custodian_id.as_ref()),
            opt_text(input.custodian_name.as_deref()),
            opt_text(spec.location.as_deref()),
            Value::Text(spec.original_value.to_string()),
            Value::Text(spec.tax_amount.to_string()),
            Value::Text(input.residual_value.to_string()),
            Value::Text(input.depreciable_base.to_string()),
            Value::from(input.useful_life_months),
            Value::Text(input.salvage_rate.to_string()),
            Value::Text(input.monthly_depreciation.to_string()),
            Value::Text(input.start_depreciation_date.to_string()),
            Value::Text(spec.original_value.to_string()),
            Value::Text(spec.purchase_date.to_string()),
            Value::Text(spec.source_type.clone()),
            opt_uuid(spec.source_id.as_ref()),
            opt_text(spec.source_no.as_deref()),
            Value::Text(spec.buy_mode.clone()),
            opt_uuid(spec.supplier_id.as_ref()),
            opt_text(input.supplier_name.as_deref()),
            opt_text(spec.remark.as_deref()),
            opt_text(input.operator.map(|id| id.to_string()).as_deref()),
            opt_text(input.operator.map(|id| id.to_string()).as_deref()),
        ],
    )
    .await?;
    Ok(())
}

/// 回写购入凭证。
pub async fn asset_set_voucher(ex: &impl SqlExecutor, id: Uuid, voucher_id: Uuid) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主表写入。
    ex.exec_sql(
        format!("UPDATE {TABLE} SET voucher_id = ?::uuid, updated_at = ? WHERE id = ?::uuid"),
        vec![Value::from(&voucher_id), Value::from(Utc::now()), Value::from(&id)],
    )
    .await
}

/// 修改卡片的可改字段（名称 / 规格 / 地点 / 备注）。
pub async fn asset_update(
    ex: &impl SqlExecutor,
    id: Uuid,
    name: Option<&str>,
    spec: Option<&str>,
    location: Option<&str>,
    remark: Option<&str>,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主表写入。
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET \
                asset_name = COALESCE(NULLIF(?::text,''), asset_name), \
                spec = COALESCE(?::text, spec), \
                location = COALESCE(?::text, location), \
                remark = COALESCE(?::text, remark), \
                updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![
            opt_text(name),
            opt_text(spec),
            opt_text(location),
            opt_text(remark),
            Value::from(Utc::now()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            Value::from(&id),
        ],
    )
    .await
}

/// 软删卡片。
pub async fn asset_soft_delete(ex: &impl SqlExecutor, id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主表写入。
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET deleted_at = ?, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![
            Value::from(Utc::now()),
            Value::from(Utc::now()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            Value::from(&id),
        ],
    )
    .await
}

/// 作废处置：状态恢复在役 + 清处置三列（**一条 UPDATE**）。
///
/// 必须一条语句：`ck_biz_fa_disposed` 要求「`status='DISPOSED'` ⇔ 处置日与类型都非空」，
/// 分两步写（先改状态、再清列）中间那一刻必然违反约束（V0230 第一次跑就是这么挂的）。
/// 顺序也固定为 CAS 到 `DISPOSED`，防两次「作废处置」并发。
pub async fn asset_restore_from_disposal(ex: &impl SqlExecutor, id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主表写入。
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET status = 'IN_USE', disposal_type = NULL, disposal_date = NULL, \
                    disposal_voucher_id = NULL, updated_at = ?, \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status = 'DISPOSED' AND deleted_at IS NULL"
        ),
        vec![
            Value::from(Utc::now()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            Value::from(&id),
        ],
    )
    .await
}

/// 部门转移：覆盖主表当前值（CAS 到 `from_status`，防并发改两次）。
pub async fn asset_set_dept(
    ex: &impl SqlExecutor,
    id: Uuid,
    from_status: &str,
    to_dept_id: Uuid,
    to_dept_name: &str,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主表写入。
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET use_dept_id = ?::uuid, use_dept_name = ?, updated_at = ?, \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
        ),
        vec![
            Value::from(&to_dept_id),
            Value::Text(to_dept_name.to_string()),
            Value::from(Utc::now()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            Value::from(&id),
            Value::Text(from_status.to_string()),
        ],
    )
    .await
}

/// 使用人变更：覆盖主表当前值。
pub async fn asset_set_custodian(
    ex: &impl SqlExecutor,
    id: Uuid,
    from_status: &str,
    to_user_id: Uuid,
    to_user_name: &str,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主表写入。
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET custodian_id = ?::uuid, custodian_name = ?, updated_at = ?, \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
        ),
        vec![
            Value::from(&to_user_id),
            Value::Text(to_user_name.to_string()),
            Value::from(Utc::now()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            Value::from(&id),
            Value::Text(from_status.to_string()),
        ],
    )
    .await
}

/// 状态 CAS 回写（预期状态 → 目标状态，返回 0 = 状态在别处被改过）。
///
/// 只改 `status`：处置的额外三列（`disposal_type / disposal_date /
/// disposal_voucher_id`）由 [`asset_mark_disposed`] 写，作废处置由
/// [`asset_clear_disposal`] 清 —— 三者分开是为了「状态已经前进但凭证还没落」
/// 这种中间态能一眼看出是哪个环节失败的。
pub async fn apply_asset_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    from: &str,
    to: &str,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主表写入。
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET status = ?, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(to.to_string()),
            Value::from(Utc::now()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            Value::from(&id),
            Value::Text(from.to_string()),
        ],
    )
    .await
}

/// 标记已处置（状态 + 处置三列，CAS 到在役 / 闲置）。
pub async fn asset_mark_disposed(
    ex: &impl SqlExecutor,
    id: Uuid,
    disposal_type: &str,
    disposal_date: NaiveDate,
    first_voucher_id: Option<Uuid>,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主表写入。
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET status = 'DISPOSED', disposal_type = ?, disposal_date = ?::text::date, \
                    disposal_voucher_id = NULLIF(?::text,'')::uuid, updated_at = ?, \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status IN ('IN_USE','IDLE') AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(disposal_type.to_string()),
            Value::Text(disposal_date.to_string()),
            opt_uuid(first_voucher_id.as_ref()),
            Value::from(Utc::now()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            Value::from(&id),
        ],
    )
    .await
}

/// 清处置痕迹（作废处置后恢复在役用）。
pub async fn asset_clear_disposal(ex: &impl SqlExecutor, id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主表写入。
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET disposal_type = NULL, disposal_date = NULL, \
                    disposal_voucher_id = NULL, updated_at = ?, \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![
            Value::from(Utc::now()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            Value::from(&id),
        ],
    )
    .await
}

// ==================== ③ 变动留痕 ====================

/// 变动行。
#[derive(Debug, Clone)]
pub struct AssetChangeRow {
    /// 主键。
    pub id: Uuid,
    /// 资产 id。
    pub asset_id: Uuid,
    /// 变动类型。
    pub change_type: String,
    /// 变动日期。
    pub change_date: NaiveDate,
    /// 原部门 id。
    pub from_dept_id: Option<Uuid>,
    /// 原部门名。
    pub from_dept_name: Option<String>,
    /// 新部门 id。
    pub to_dept_id: Option<Uuid>,
    /// 新部门名。
    pub to_dept_name: Option<String>,
    /// 原使用人 id。
    pub from_user_id: Option<Uuid>,
    /// 原使用人名。
    pub from_user_name: Option<String>,
    /// 新使用人 id。
    pub to_user_id: Option<Uuid>,
    /// 新使用人名。
    pub to_user_name: Option<String>,
    /// 原状态。
    pub from_status: Option<String>,
    /// 新状态。
    pub to_status: Option<String>,
    /// 原因。
    pub reason: String,
    /// 凭证。
    pub voucher_id: Option<Uuid>,
    /// 审批实例。
    pub approval_instance_id: Option<Uuid>,
    /// 操作人。
    pub operator_name: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 时间。
    pub created_at: DateTime<Utc>,
}

const CHANGE_COLUMNS: &str = "c.id, c.asset_id, c.change_type, c.change_date, c.from_dept_id, \
     c.from_dept_name, c.to_dept_id, c.to_dept_name, c.from_user_id, c.from_user_name, \
     c.to_user_id, c.to_user_name, c.from_status, c.to_status, c.reason, c.voucher_id, \
     c.approval_instance_id, c.operator_name, c.remark, c.created_at";

impl AssetChangeRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            asset_id: row.get_typed("asset_id")?,
            change_type: row.get_typed("change_type")?,
            change_date: row.get_typed("change_date")?,
            from_dept_id: row.get_typed("from_dept_id")?,
            from_dept_name: row.get_typed("from_dept_name")?,
            to_dept_id: row.get_typed("to_dept_id")?,
            to_dept_name: row.get_typed("to_dept_name")?,
            from_user_id: row.get_typed("from_user_id")?,
            from_user_name: row.get_typed("from_user_name")?,
            to_user_id: row.get_typed("to_user_id")?,
            to_user_name: row.get_typed("to_user_name")?,
            from_status: row.get_typed("from_status")?,
            to_status: row.get_typed("to_status")?,
            reason: row.get_typed("reason")?,
            voucher_id: row.get_typed("voucher_id")?,
            approval_instance_id: row.get_typed("approval_instance_id")?,
            operator_name: row.get_typed("operator_name")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
        })
    }
}

/// 写一行变动留痕（append-only）。
///
/// 参数多但都是「这一次变动的事实」：谁 → 谁、什么 → 什么、为什么。
/// 用结构体入参是为了让调用点读起来像一张表（而不是 12 个位置参数）。
#[derive(Debug, Clone)]
pub struct ChangeInsert<'a> {
    /// 变动类型。
    pub change_type: &'a str,
    /// 变动日期。
    pub change_date: NaiveDate,
    /// 原部门 id。
    pub from_dept_id: Option<Uuid>,
    /// 原部门名。
    pub from_dept_name: Option<&'a str>,
    /// 新部门 id。
    pub to_dept_id: Option<Uuid>,
    /// 新部门名。
    pub to_dept_name: Option<&'a str>,
    /// 原使用人 id。
    pub from_user_id: Option<Uuid>,
    /// 原使用人名。
    pub from_user_name: Option<&'a str>,
    /// 新使用人 id。
    pub to_user_id: Option<Uuid>,
    /// 新使用人名。
    pub to_user_name: Option<&'a str>,
    /// 原状态。
    pub from_status: Option<&'a str>,
    /// 新状态。
    pub to_status: Option<&'a str>,
    /// 原因（必填）。
    pub reason: &'a str,
    /// 凭证。
    pub voucher_id: Option<Uuid>,
    /// 审批实例。
    pub approval_instance_id: Option<Uuid>,
    /// 操作人 id。
    pub operator_id: Option<Uuid>,
    /// 操作人名。
    pub operator_name: Option<&'a str>,
    /// 备注。
    pub remark: Option<&'a str>,
}

/// 写变动留痕。
pub async fn insert_change(ex: &impl SqlExecutor, asset_id: Uuid, input: &ChangeInsert<'_>) -> DbResult<Uuid> {
    let id = Uuid::now_v7();
    // SCOPE-EXEMPT: 明细表写入（按资产归属）。
    ex.exec_sql(
        format!(
            "INSERT INTO {CHANGE_TABLE} \
                (id, asset_id, change_type, change_date, from_dept_id, from_dept_name, \
                 to_dept_id, to_dept_name, from_user_id, from_user_name, to_user_id, to_user_name, \
                 from_status, to_status, reason, voucher_id, approval_instance_id, operator_id, \
                 operator_name, remark) \
             VALUES (?::uuid, ?::uuid, ?, ?::text::date, NULLIF(?::text,'')::uuid, NULLIF(?::text,''), \
                     NULLIF(?::text,'')::uuid, NULLIF(?::text,''), NULLIF(?::text,'')::uuid, \
                     NULLIF(?::text,''), NULLIF(?::text,'')::uuid, NULLIF(?::text,''), \
                     NULLIF(?::text,''), NULLIF(?::text,''), ?, NULLIF(?::text,'')::uuid, \
                     NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid, NULLIF(?::text,''), \
                     NULLIF(?::text,''))"
        ),
        vec![
            Value::from(&id),
            Value::from(&asset_id),
            Value::Text(input.change_type.to_string()),
            Value::Text(input.change_date.to_string()),
            opt_uuid(input.from_dept_id.as_ref()),
            opt_text(input.from_dept_name),
            opt_uuid(input.to_dept_id.as_ref()),
            opt_text(input.to_dept_name),
            opt_uuid(input.from_user_id.as_ref()),
            opt_text(input.from_user_name),
            opt_uuid(input.to_user_id.as_ref()),
            opt_text(input.to_user_name),
            opt_text(input.from_status),
            opt_text(input.to_status),
            Value::Text(input.reason.to_string()),
            opt_uuid(input.voucher_id.as_ref()),
            opt_uuid(input.approval_instance_id.as_ref()),
            opt_uuid(input.operator_id.as_ref()),
            opt_text(input.operator_name),
            opt_text(input.remark),
        ],
    )
    .await?;
    Ok(id)
}

/// 某资产的变动时间线（倒序）。
pub async fn changes_of(asset_id: Uuid) -> DbResult<Vec<AssetChangeRow>> {
    // SCOPE-EXEMPT: 按资产 id 取明细行。
    let rows = db()
        .find_with_params(
            &format!(
                "SELECT {CHANGE_COLUMNS} FROM {CHANGE_TABLE} c \
                  WHERE c.asset_id = ?::uuid ORDER BY c.change_date DESC, c.created_at DESC"
            ),
            vec![Value::from(&asset_id)],
        )
        .await?;
    rows.iter().map(AssetChangeRow::from_row).collect()
}

// ==================== ③' 期间归属部门（补提历史折旧用） ====================

/// 部门快照（只用于「这一期属于哪个部门」的还原）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeptSnapshot {
    /// 部门 id。
    pub dept_id: Uuid,
    /// 部门名（`biz_fixed_asset_change` 的 `*_dept_name` 快照列）。
    ///
    /// 留痕写入时一定带上名字（`asset_handler::asset_transfer` 先查 `sys_dept` 再写），
    /// 但列本身可空（历史数据 / 手插行），所以是 `Option`：解析时兜底查 `sys_dept`，
    /// 再兜底给一个带 id 前 8 位的占位名（绝不能回落到「卡片当前部门名」—— 那会让
    /// 摘要读起来像另一件事）。
    pub dept_name: Option<String>,
}

impl DeptSnapshot {
    /// 摘要/日志里用的部门名：快照名 → 占位名（`部门 <uuid 前 8 位>`）。
    fn display_name(&self) -> String {
        match self.dept_name.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
            Some(name) => name.to_string(),
            None => {
                let text = self.dept_id.to_string();
                format!("部门 {}", text.get(..8).unwrap_or(&text))
            }
        }
    }
}

/// 「某台资产在某个期末属于哪个部门」的还原线索。
///
/// 两个端点各取一次（都是 `DISTINCT ON`，一次批量查询，**不随资产数增加往返**）：
///
/// * [`Self::at_or_before`]：`change_date <= period_end` 的**最后一次** TRANSFER 的**转入**部门；
/// * [`Self::first_after`]：`change_date > period_end` 的**第一次** TRANSFER 的**转出**部门。
#[derive(Debug, Clone, Default)]
pub struct DeptAtPeriod {
    /// 期末（含）前最后一次转移的转入部门。键 = 资产 id。
    pub at_or_before: HashMap<Uuid, DeptSnapshot>,
    /// 期末之后第一次转移的转出部门。键 = 资产 id。
    pub first_after: HashMap<Uuid, DeptSnapshot>,
}

impl DeptAtPeriod {
    /// 资产在 `period_end` 那天结束时所属的部门 → `(部门 id, 部门名)`。
    ///
    /// 优先级（卡 `t_abb4bb0b` 的边界口径，逐条对应）：
    ///
    /// 1. **期末（含）之前有过转移** → 最后一次转移的**转入**部门。
    ///    期间内转移（如 2/15 转）按 `change_date <= period_end` 判：整月算新部门 ——
    ///    这是「费用归属」口径，与「当月减少当月照提」的**资产**口径不是一回事
    ///    （后者决定提不提，前者决定记哪个部门）。
    /// 2. **期末之后才转移** → 那次转移的**转出**部门（那一期还没转出去，用的是旧部门）。
    ///    没有这一条，补提「转移前那个月」会退化成卡片当前部门（= 新部门），本卡的 bug 就还在。
    /// 3. **从没转移过** → 卡片当前 `use_dept_id`（保持原行为，无转移的资产完全不受影响）。
    pub fn resolve(&self, asset_id: &Uuid, current_dept_id: Uuid, current_dept_name: &str) -> (Uuid, String) {
        match self
            .at_or_before
            .get(asset_id)
            .or_else(|| self.first_after.get(asset_id))
        {
            Some(snapshot) => (snapshot.dept_id, snapshot.display_name()),
            None => (current_dept_id, current_dept_name.to_string()),
        }
    }
}

/// `{a,b,c}` 形态的 uuid 数组字面量（`ANY(?::text::uuid[])` 用）。
fn uuid_array(ids: &[Uuid]) -> Value {
    let joined = ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
    Value::Text(format!("{{{joined}}}"))
}

/// 批量取「这些资产在 `period_end` 时属于哪个部门」的线索（**只取 TRANSFER 留痕**）。
///
/// 两次批量查询（不是每台一次）：折旧批跑一次几百台，逐台查转移留痕就是 N+1。
/// 名字优先用留痕快照（`to_dept_name` / `from_dept_name`），为空再 left join
/// `sys_dept` 取现名（**不过滤 `deleted_at`**：部门被删了也要读得出当时叫什么）。
pub async fn dept_at_period_hint(
    ex: &impl SqlExecutor,
    asset_ids: &[Uuid],
    period_end: NaiveDate,
) -> DbResult<DeptAtPeriod> {
    let mut hint = DeptAtPeriod::default();
    if asset_ids.is_empty() {
        return Ok(hint);
    }
    let ids = uuid_array(asset_ids);
    let end = Value::Text(period_end.to_string());
    // SCOPE-EXEMPT: 折旧批跑按资产 id 集合取变动留痕（不分页、不产生行集权限）。
    let before = ex
        .query_sql(
            format!(
                "SELECT DISTINCT ON (c.asset_id) c.asset_id, c.to_dept_id AS dept_id, \
                        COALESCE(NULLIF(c.to_dept_name, ''), d.dept_name) AS dept_name \
                   FROM {CHANGE_TABLE} c LEFT JOIN sys_dept d ON d.id = c.to_dept_id \
                  WHERE c.asset_id = ANY(?::text::uuid[]) \
                    AND c.change_type = ? \
                    AND c.change_date <= ?::text::date \
                  ORDER BY c.asset_id, c.change_date DESC, c.created_at DESC"
            ),
            vec![
                ids.clone(),
                Value::Text(asset_dto::CHANGE_TRANSFER.to_string()),
                end.clone(),
            ],
        )
        .await?;
    for row in before.iter() {
        let Some(dept_id) = row.get_typed::<Option<Uuid>>("dept_id")? else {
            continue;
        };
        hint.at_or_before.insert(
            row.get_typed("asset_id")?,
            DeptSnapshot {
                dept_id,
                dept_name: row.get_typed("dept_name")?,
            },
        );
    }
    // SCOPE-EXEMPT: 同上（期末之后第一次转移的转出部门）。
    let after = ex
        .query_sql(
            format!(
                "SELECT DISTINCT ON (c.asset_id) c.asset_id, c.from_dept_id AS dept_id, \
                        COALESCE(NULLIF(c.from_dept_name, ''), d.dept_name) AS dept_name \
                   FROM {CHANGE_TABLE} c LEFT JOIN sys_dept d ON d.id = c.from_dept_id \
                  WHERE c.asset_id = ANY(?::text::uuid[]) \
                    AND c.change_type = ? \
                    AND c.change_date > ?::text::date \
                  ORDER BY c.asset_id, c.change_date ASC, c.created_at ASC"
            ),
            vec![ids, Value::Text(asset_dto::CHANGE_TRANSFER.to_string()), end],
        )
        .await?;
    for row in after.iter() {
        let Some(dept_id) = row.get_typed::<Option<Uuid>>("dept_id")? else {
            continue;
        };
        hint.first_after.insert(
            row.get_typed("asset_id")?,
            DeptSnapshot {
                dept_id,
                dept_name: row.get_typed("dept_name")?,
            },
        );
    }
    Ok(hint)
}

// ==================== ④ 折旧批次 ====================

/// 折旧批次行。
#[derive(Debug, Clone)]
pub struct DepBatchRow {
    /// 主键。
    pub id: Uuid,
    /// 批次号。
    pub batch_no: String,
    /// 期间。
    pub period_code: String,
    /// 状态。
    pub status: String,
    /// 资产台数。
    pub asset_count: i32,
    /// 合计金额。
    pub total_amount: Decimal,
    /// 计提凭证。
    pub voucher_id: Option<Uuid>,
    /// 红冲凭证。
    pub reverse_voucher_id: Option<Uuid>,
    /// 跳过原因。
    pub skip_reason: Option<String>,
    /// 运行时间。
    pub run_at: DateTime<Utc>,
    /// 过账时间。
    pub posted_at: Option<DateTime<Utc>>,
    /// 红冲时间。
    pub reversed_at: Option<DateTime<Utc>>,
    /// 备注。
    pub remark: Option<String>,
}

const BATCH_COLUMNS: &str = "b.id, b.batch_no, b.period_code, b.status, b.asset_count, \
     b.total_amount::text AS total_amount, b.voucher_id, b.reverse_voucher_id, b.skip_reason, \
     b.run_at, b.posted_at, b.reversed_at, b.remark";

impl DepBatchRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            batch_no: row.get_typed("batch_no")?,
            period_code: row.get_typed("period_code")?,
            status: row.get_typed("status")?,
            asset_count: row.get_typed("asset_count")?,
            total_amount: parse_amount(&row.get_typed::<String>("total_amount")?),
            voucher_id: row.get_typed("voucher_id")?,
            reverse_voucher_id: row.get_typed("reverse_voucher_id")?,
            skip_reason: row.get_typed("skip_reason")?,
            run_at: row.get_typed("run_at")?,
            posted_at: row.get_typed("posted_at")?,
            reversed_at: row.get_typed("reversed_at")?,
            remark: row.get_typed("remark")?,
        })
    }
}

/// 按期间取批次（`uk_biz_dep_batch_period` 保证至多一行）。
pub async fn batch_by_period(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Option<DepBatchRow>> {
    // SCOPE-EXEMPT: 按期间精确取 1 行。
    let rows = ex
        .query_sql(
            format!("SELECT {BATCH_COLUMNS} FROM {DEP_BATCH_TABLE} b WHERE b.period_code = ?"),
            vec![Value::Text(period_code.to_string())],
        )
        .await?;
    rows.first().map(DepBatchRow::from_row).transpose()
}

/// 按 id 取批次。
pub async fn batch_find(id: Uuid) -> DbResult<Option<DepBatchRow>> {
    batch_find_in(db(), id).await
}

/// 按 id 取批次（同事务版）。
pub async fn batch_find_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<DepBatchRow>> {
    // SCOPE-EXEMPT: 主键精确取 1 行。
    let rows = ex
        .query_sql(
            format!("SELECT {BATCH_COLUMNS} FROM {DEP_BATCH_TABLE} b WHERE b.id = ?::uuid"),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(DepBatchRow::from_row).transpose()
}

/// 批次分页。
pub async fn batch_page(q: &DepBatchFilter, page: u64, page_size: u64) -> DbResult<(Vec<DepBatchRow>, i64)> {
    // SCOPE-EXEMPT: 批次是期间台账，无归属列。
    let mut sql = format!("SELECT {BATCH_COLUMNS} FROM {DEP_BATCH_TABLE} b WHERE 1 = 1");
    let mut params: Vec<Value> = Vec::new();
    if let Some(period) = q.period_code.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        sql.push_str(" AND b.period_code = ?");
        params.push(Value::Text(period.to_string()));
    }
    if let Some(status) = q.status.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        sql.push_str(" AND b.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    let order = "ORDER BY b.period_code DESC";
    let result = scope::paginate_unscoped(BATCH_TARGET, &sql, params, Some(order), page, page_size).await?;
    let rows = result
        .list
        .iter()
        .map(DepBatchRow::from_row)
        .collect::<DbResult<Vec<_>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// [`batch_insert`] 的入参（`ex` 之外的 7 项）。
#[derive(Debug, Clone, Copy)]
pub struct DepBatchInsertInput<'a> {
    /// 批次主键（调用方生成，明细 / 卡片回写都用它）。
    pub id: Uuid,
    /// 批次号（`next_batch_no` 生成）。
    pub batch_no: &'a str,
    /// 计提期间编码（`YYYY-MM`，与批次唯一键 `uk_biz_dep_batch_period` 相关）。
    pub period_code: &'a str,
    /// 本期计提资产台数（合法空跑时为 0）。
    pub asset_count: i32,
    /// 本期计提金额合计。
    pub total_amount: Decimal,
    /// 操作人（落 `run_by`；`None` = 系统 / 测试）。
    pub operator: Option<Uuid>,
    /// 备注（`None`/空 = 不填）。
    pub remark: Option<&'a str>,
}

/// 插入批次（初始 `CALCULATED`；调用方随后要么 `POSTED` 要么 `SKIPPED`）。
pub async fn batch_insert(ex: &impl SqlExecutor, input: DepBatchInsertInput<'_>) -> DbResult<()> {
    let DepBatchInsertInput {
        id,
        batch_no,
        period_code,
        asset_count,
        total_amount,
        operator,
        remark,
    } = input;
    // SCOPE-EXEMPT: 主表写入。
    ex.exec_sql(
        format!(
            "INSERT INTO {DEP_BATCH_TABLE} \
                (id, batch_no, period_code, status, asset_count, total_amount, run_by, remark) \
             VALUES (?::uuid, ?, ?, 'CALCULATED', ?::int, ?::text::numeric, NULLIF(?::text,'')::uuid, \
                     NULLIF(?::text,''))"
        ),
        vec![
            Value::from(&id),
            Value::Text(batch_no.to_string()),
            Value::Text(period_code.to_string()),
            Value::from(asset_count),
            Value::Text(total_amount.to_string()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            opt_text(remark),
        ],
    )
    .await?;
    Ok(())
}

/// [`batch_set_status`] 的入参（`ex` 之外的 7 项）。
#[derive(Debug, Clone, Copy)]
pub struct DepBatchStatusInput<'a> {
    /// 批次主键（`WHERE id = ?::uuid`）。
    pub id: Uuid,
    /// 期望的当前态（CAS：`WHERE status = ?`）。
    pub from: &'a str,
    /// 目标态（`CALCULATED` / `POSTED` / `SKIPPED` / `REVERSED`）。
    pub to: &'a str,
    /// 蓝字凭证 id（仅 `→ POSTED` 用）。
    pub voucher_id: Option<Uuid>,
    /// 红字凭证 id（仅 `→ REVERSED` 用）。
    pub reverse_voucher_id: Option<Uuid>,
    /// 跳过原因（仅 `→ SKIPPED` 用）。
    pub skip_reason: Option<&'a str>,
    /// 业务时间（`POSTED` / `REVERSED` 的时间戳由它给）。
    pub now: DateTime<Utc>,
}

/// 更新批次状态（CAS 到 `from`）。
pub async fn batch_set_status(ex: &impl SqlExecutor, input: DepBatchStatusInput<'_>) -> DbResult<u64> {
    let DepBatchStatusInput {
        id,
        from,
        to,
        voucher_id,
        reverse_voucher_id,
        skip_reason,
        now,
    } = input;
    // SCOPE-EXEMPT: 主表写入。
    //
    // 时间戳由调用方给（不在 SQL 里写 `CASE WHEN ? = 'POSTED' THEN now()`）：
    // 那种写法让同一个占位符既当字符串比较又当时间来源，驱动按值精确校验类型时
    // 会报「query failed: db error」（没有可读信息，排查成本高）。这里全部当 text 传，
    // 由 `NULLIF(...)::timestamptz` 转，类型路径唯一。
    let posted_at = if to == crate::business::erp::asset_dto::DEP_POSTED {
        now.to_rfc3339()
    } else {
        String::new()
    };
    let reversed_at = if to == crate::business::erp::asset_dto::DEP_REVERSED {
        now.to_rfc3339()
    } else {
        String::new()
    };
    ex.exec_sql(
        format!(
            "UPDATE {DEP_BATCH_TABLE} SET status = ?, \
                    voucher_id = COALESCE(NULLIF(?::text,'')::uuid, voucher_id), \
                    reverse_voucher_id = COALESCE(NULLIF(?::text,'')::uuid, reverse_voucher_id), \
                    skip_reason = COALESCE(?::text, skip_reason), \
                    posted_at = COALESCE(NULLIF(?::text,'')::timestamptz, posted_at), \
                    reversed_at = COALESCE(NULLIF(?::text,'')::timestamptz, reversed_at), \
                    updated_at = now() \
              WHERE id = ?::uuid AND status = ?"
        ),
        vec![
            Value::Text(to.to_string()),
            opt_uuid(voucher_id.as_ref()),
            opt_uuid(reverse_voucher_id.as_ref()),
            opt_text(skip_reason),
            Value::Text(posted_at),
            Value::Text(reversed_at),
            Value::from(&id),
            Value::Text(from.to_string()),
        ],
    )
    .await
}

/// 重跑时把批次拉回 `CALCULATED` 并清掉本次的台数 / 金额（`voucher_id` 换成新凭证，
/// `reverse_voucher_id` 保留上一次的红字凭证指针 —— 历史链靠凭证的
/// `reverse_of / reversed_by` 追溯，见 ARCH-6 §5.5.5）。
pub async fn batch_reset_for_rerun(
    ex: &impl SqlExecutor,
    id: Uuid,
    asset_count: i32,
    total_amount: Decimal,
    remark: Option<&str>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主表写入。
    ex.exec_sql(
        format!(
            "UPDATE {DEP_BATCH_TABLE} SET asset_count = ?::int, total_amount = ?::text::numeric, \
                    voucher_id = NULL, posted_at = NULL, reversed_at = NULL, \
                    remark = COALESCE(?::text, remark), updated_at = now() \
              WHERE id = ?::uuid"
        ),
        vec![
            Value::from(asset_count),
            Value::Text(total_amount.to_string()),
            opt_text(remark),
            Value::from(&id),
        ],
    )
    .await
}

/// 删除某批次下的全部明细（重跑前清掉上一轮的草稿行）。
///
/// 只在 `REVERSED → CALCULATED` 重跑时调用：上一轮明细已全部 `REVERSED`，
/// 它们的金额已经从卡片上回滚过，物理删掉不会留下「无法解释的明细」。
pub async fn batch_clear_details(ex: &impl SqlExecutor, batch_id: Uuid) -> DbResult<u64> {
    // SCOPE-EXEMPT: 明细表，按批次 id 删。
    ex.exec_sql(
        format!("DELETE FROM {DEP_TABLE} WHERE batch_id = ?::uuid"),
        vec![Value::from(&batch_id)],
    )
    .await
}

// ==================== ⑤ 折旧明细 ====================

/// 折旧明细行。
#[derive(Debug, Clone)]
pub struct DepDetailRow {
    /// 主键。
    pub id: Uuid,
    /// 批次。
    pub batch_id: Uuid,
    /// 资产。
    pub asset_id: Uuid,
    /// 资产编号（快照）。
    pub asset_no: String,
    /// 资产名称（快照）。
    pub asset_name: String,
    /// 期间。
    pub period_code: String,
    /// 部门。
    pub dept_id: Uuid,
    /// 部门名（快照）。
    pub dept_name: String,
    /// 费用科目。
    pub expense_account_id: Uuid,
    /// 费用科目编码。
    pub expense_account_code: String,
    /// 本月折旧。
    pub amount: Decimal,
    /// 计提前累计。
    pub accumulated_before: Decimal,
    /// 计提后累计。
    pub accumulated_after: Decimal,
    /// 计提后净值。
    pub net_value_after: Decimal,
    /// 是否尾差补齐期。
    pub is_last_month: i16,
    /// 状态。
    pub status: String,
}

const DEP_COLUMNS: &str = "d.id, d.batch_id, d.asset_id, d.asset_no, d.asset_name, d.period_code, \
     d.dept_id, d.dept_name, d.expense_account_id, d.expense_account_code, \
     d.amount::text AS amount, d.accumulated_before::text AS accumulated_before, \
     d.accumulated_after::text AS accumulated_after, d.net_value_after::text AS net_value_after, \
     d.is_last_month, d.status";

impl DepDetailRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            batch_id: row.get_typed("batch_id")?,
            asset_id: row.get_typed("asset_id")?,
            asset_no: row.get_typed("asset_no")?,
            asset_name: row.get_typed("asset_name")?,
            period_code: row.get_typed("period_code")?,
            dept_id: row.get_typed("dept_id")?,
            dept_name: row.get_typed("dept_name")?,
            expense_account_id: row.get_typed("expense_account_id")?,
            expense_account_code: row.get_typed("expense_account_code")?,
            amount: parse_amount(&row.get_typed::<String>("amount")?),
            accumulated_before: parse_amount(&row.get_typed::<String>("accumulated_before")?),
            accumulated_after: parse_amount(&row.get_typed::<String>("accumulated_after")?),
            net_value_after: parse_amount(&row.get_typed::<String>("net_value_after")?),
            is_last_month: row.get_typed("is_last_month")?,
            status: row.get_typed("status")?,
        })
    }
}

/// 一条折旧明细的写入参数。
#[derive(Debug, Clone)]
pub struct DepInsert {
    /// 资产 id。
    pub asset_id: Uuid,
    /// 资产编号。
    pub asset_no: String,
    /// 资产名称。
    pub asset_name: String,
    /// 部门 id。
    pub dept_id: Uuid,
    /// 部门名。
    pub dept_name: String,
    /// 费用科目 id。
    pub expense_account_id: Uuid,
    /// 费用科目编码。
    pub expense_account_code: String,
    /// 本月折旧。
    pub amount: Decimal,
    /// 计提前累计。
    pub accumulated_before: Decimal,
    /// 计提后累计。
    pub accumulated_after: Decimal,
    /// 计提后净值。
    pub net_value_after: Decimal,
    /// 是否尾差补齐期。
    pub is_last_month: bool,
}

/// 写一条折旧明细。
pub async fn insert_depreciation(
    ex: &impl SqlExecutor,
    batch_id: Uuid,
    period_code: &str,
    input: &DepInsert,
) -> DbResult<Uuid> {
    let id = Uuid::now_v7();
    // SCOPE-EXEMPT: 明细表写入。
    ex.exec_sql(
        format!(
            "INSERT INTO {DEP_TABLE} \
                (id, batch_id, asset_id, asset_no, asset_name, period_code, dept_id, dept_name, \
                 expense_account_id, expense_account_code, amount, accumulated_before, \
                 accumulated_after, net_value_after, is_last_month, status) \
             VALUES (?::uuid, ?::uuid, ?::uuid, ?, ?, ?, ?::uuid, ?, ?::uuid, ?, ?::text::numeric, \
                     ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::int, 'POSTED')"
        ),
        vec![
            Value::from(&id),
            Value::from(&batch_id),
            Value::from(&input.asset_id),
            Value::Text(input.asset_no.clone()),
            Value::Text(input.asset_name.clone()),
            Value::Text(period_code.to_string()),
            Value::from(&input.dept_id),
            Value::Text(input.dept_name.clone()),
            Value::from(&input.expense_account_id),
            Value::Text(input.expense_account_code.clone()),
            Value::Text(input.amount.to_string()),
            Value::Text(input.accumulated_before.to_string()),
            Value::Text(input.accumulated_after.to_string()),
            Value::Text(input.net_value_after.to_string()),
            Value::from(i32::from(input.is_last_month)),
        ],
    )
    .await?;
    Ok(id)
}

/// 某批次的明细（按资产编号）。
pub async fn details_of(batch_id: Uuid) -> DbResult<Vec<DepDetailRow>> {
    // SCOPE-EXEMPT: 按批次 id 取明细。
    let rows = db()
        .find_with_params(
            &format!(
                "SELECT {DEP_COLUMNS} FROM {DEP_TABLE} d WHERE d.batch_id = ?::uuid \
                  ORDER BY d.asset_no ASC"
            ),
            vec![Value::from(&batch_id)],
        )
        .await?;
    rows.iter().map(DepDetailRow::from_row).collect()
}

/// 某资产的折旧明细（倒序，详情页时间线）。
pub async fn details_of_asset(asset_id: Uuid) -> DbResult<Vec<DepDetailRow>> {
    // SCOPE-EXEMPT: 按资产 id 取明细。
    let rows = db()
        .find_with_params(
            &format!(
                "SELECT {DEP_COLUMNS} FROM {DEP_TABLE} d WHERE d.asset_id = ?::uuid \
                  ORDER BY d.period_code DESC"
            ),
            vec![Value::from(&asset_id)],
        )
        .await?;
    rows.iter().map(DepDetailRow::from_row).collect()
}

/// 某台资产在某个会计期间的折旧明细**状态**（`None` = 这一期没有它的明细行）。
///
/// 只取「一台一期」这一行（`uk_biz_dep_asset_period` 唯一），给处置前置闸用
/// （`asset_compute::disposal_dep_gate` / `asset_handler::asset_dispose`）：
/// 处置当月的折旧必须先计提并过账，否则月中处置的资产会漏提一个月。
///
/// 返回 `Some("POSTED")` / `Some("REVERSED")`（取自 `biz_depreciation.status`，
/// 取值域见 V0230 `ck_biz_dep_status`）/ `None`（没有明细）。
pub async fn depreciation_status_of(asset_id: Uuid, period_code: &str) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: 按 (资产, 期间) 唯一键取 1 行状态，与「谁能看到这张卡」无关。
    let rows = db()
        .find_with_params(
            &format!("SELECT status FROM {DEP_TABLE} WHERE asset_id = ?::uuid AND period_code = ?"),
            vec![Value::from(&asset_id), Value::Text(period_code.to_string())],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => row.get_typed("status"),
    }
}

/// 把批次明细标红冲（`POSTED → REVERSED`），返回受影响行数。
pub async fn reverse_details(ex: &impl SqlExecutor, batch_id: Uuid) -> DbResult<u64> {
    // SCOPE-EXEMPT: 明细表按批次改。
    ex.exec_sql(
        format!(
            "UPDATE {DEP_TABLE} SET status = 'REVERSED', reversed_at = now() \
              WHERE batch_id = ?::uuid AND status = 'POSTED'"
        ),
        vec![Value::from(&batch_id)],
    )
    .await
}

/// 回滚卡片折旧缓存列（红冲用：`accumulated -= amount` / `net += amount` / `months - 1`）。
///
/// `last_depreciation_period` 回退到「该资产最大的 POSTED 期间」——
/// 不能简单取上一期：中途红冲的不是最后一期时（先红冲 9 月、10 月还在），
/// 简单减一会把它退到 09，下次 10 月重跑就会撞 `uk_biz_dep_asset_period`。
pub async fn rollback_depreciation_on_asset(ex: &impl SqlExecutor, asset_id: Uuid, amount: Decimal) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主表写入。
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} a SET \
                accumulated_depreciation = a.accumulated_depreciation - ?::text::numeric, \
                net_value = a.original_value - (a.accumulated_depreciation - ?::text::numeric), \
                depreciation_months_done = GREATEST(a.depreciation_months_done - 1, 0), \
                last_depreciation_period = ( \
                    SELECT max(d.period_code) FROM {DEP_TABLE} d \
                     WHERE d.asset_id = a.id AND d.status = 'POSTED'), \
                updated_at = ? \
              WHERE a.id = ?::uuid AND a.deleted_at IS NULL"
        ),
        vec![
            Value::Text(amount.to_string()),
            Value::Text(amount.to_string()),
            Value::from(Utc::now()),
            Value::from(&asset_id),
        ],
    )
    .await
}

/// 折旧过账后回写卡片（**缓存列的唯一写入者**，ARCH-6 §5.5.4）。
pub async fn apply_depreciation_to_asset(
    ex: &impl SqlExecutor,
    asset_id: Uuid,
    amount: Decimal,
    accumulated_after: Decimal,
    net_value_after: Decimal,
    period_code: &str,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主表写入。
    //
    // 净值用 `original_value - accumulated` 现算而不是直接用传进来的
    // `net_value_after`：万一调用方算错，库里的 `ck_biz_fa_net`（净值恒等）
    // 会立刻拒写，而不是让「净值与累计对不上」悄悄落库。
    let _ = net_value_after;
    let _ = amount;
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} a SET \
                accumulated_depreciation = ?::text::numeric, \
                net_value = a.original_value - ?::text::numeric, \
                depreciation_months_done = a.depreciation_months_done + 1, \
                last_depreciation_period = ?, \
                updated_at = ? \
              WHERE a.id = ?::uuid AND a.deleted_at IS NULL"
        ),
        vec![
            Value::Text(accumulated_after.to_string()),
            Value::Text(accumulated_after.to_string()),
            Value::Text(period_code.to_string()),
            Value::from(Utc::now()),
            Value::from(&asset_id),
        ],
    )
    .await
}

/// 折旧扫描：目标期间的应提资产（ARCH-6 §5.5.2 的 SQL，逐条对照）。
///
/// ```sql
/// status IN ('IN_USE','IDLE')                 -- 在役 / 闲置都要提
/// start_depreciation_date <= 期末              -- 规则①：下月起提
/// accumulated < depreciable_base              -- 规则③：未提足
/// (disposal_date IS NULL OR disposal_date >= 期初)  -- 规则②：本期处置的仍要提
/// COALESCE(last_depreciation_period,'') < 期间  -- 防重复
/// ```
pub async fn scan_assets_for_period(
    ex: &impl SqlExecutor,
    period_code: &str,
    period_start: NaiveDate,
    period_end: NaiveDate,
) -> DbResult<Vec<AssetRow>> {
    // SCOPE-EXEMPT: 批跑扫描，按期间条件取行（不分页）。
    let rows = ex
        .query_sql(
            format!(
                "SELECT {ASSET_COLUMNS} FROM {TABLE} a \
                  WHERE a.deleted_at IS NULL \
                    AND a.status IN ('IN_USE','IDLE') \
                    AND a.start_depreciation_date <= ?::text::date \
                    AND a.accumulated_depreciation < a.depreciable_base \
                    AND (a.disposal_date IS NULL OR a.disposal_date >= ?::text::date) \
                    AND COALESCE(a.last_depreciation_period, '') < ? \
                  ORDER BY a.asset_no ASC"
            ),
            vec![
                Value::Text(period_end.to_string()),
                Value::Text(period_start.to_string()),
                Value::Text(period_code.to_string()),
            ],
        )
        .await?;
    rows.iter().map(AssetRow::from_row).collect()
}

/// 期间状态（`None` = 这一期还没建期间行）。
pub async fn period_status_of(period_code: &str) -> DbResult<Option<String>> {
    voucher::period_status(db(), period_code).await
}

/// 某来源单下的**全部**凭证 id（蓝字，未软删），按创建时间升序。
///
/// 折旧批次与资产处置都是「一次动作 N 张凭证」（处置出售 3 张），
/// `voucher::find_voucher_by_source` 只给第一张（`LIMIT 1`），所以这里自己取全量。
pub async fn voucher_ids_of_source(
    ex: &impl SqlExecutor,
    source_type: &str,
    source_id: Uuid,
    only_blue: bool,
) -> DbResult<Vec<Uuid>> {
    // SCOPE-EXEMPT: 凭证台账无归属列，按来源精确取。
    let mut sql = String::from(
        "SELECT id FROM biz_voucher WHERE source_type = ? AND source_id = ?::uuid \
         AND deleted_at IS NULL",
    );
    if only_blue {
        sql.push_str(" AND is_red = 0");
    }
    sql.push_str(" ORDER BY created_at ASC, voucher_no ASC");
    let rows = ex
        .query_sql(sql, vec![Value::Text(source_type.to_string()), Value::from(&source_id)])
        .await?;
    rows.iter().map(|row| row.get_typed("id")).collect()
}

#[cfg(test)]
#[path = "asset_model_tests.rs"]
mod asset_model_tests;

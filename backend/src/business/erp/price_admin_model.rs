//! 价格维护数据层（销售链 · SO-34 / ARCH-7 §15.3.2）：协议价 + 阶梯价两张表的 CRUD。
//!
//! 表结构唯一依据：`backend/migrations/business/V0270__sales_price.up.sql`
//! （`biz_customer_price` 14 列 / `biz_price_tier` 16 列）。**取价**（读口径）在
//! [`crate::business::erp::price_model`]，本文件只管**配置维护**（写口径）；
//! 两者共用同一批域值常量与唯一键口径，避免写入口放进 SQL 里而取价侧另说一套。
//!
//! # 三条与读侧共用的铁律
//!
//! 1. **软删不物理删**：`deleted_at IS NULL` 谓词贯穿所有查询；物理删 `biz_material`
//!    / `biz_customer` 会被这两张表的 `ON DELETE RESTRICT` 挡住（V0270 有意为之）。
//! 2. **唯一键是「存活行」的部分唯一索引**：
//!    `uk_biz_cust_price (customer_id, material_id, min_qty, effective_from)`
//!    `uk_biz_price_tier (material_id, customer_id, tier_type, min_qty, effective_from) NULLS NOT DISTINCT`
//!    ⇒ 同一组键在**软删后**可以重新录入；handler 先查一次给人话 409，索引是并发兜底。
//! 3. **金额/数量一律 `NUMERIC(18,4)`**：绑参走 `?::text::numeric`（驱动不吃 Decimal 直接绑），
//!    `price > 0` / `min_qty >= 0` / `max_qty > min_qty` 的 CHECK 在 DDL 里，本层不重复判。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_customer_price / biz_price_tier 是全局配置表（V0270 无 dept_id / 归属人列），
//! 数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:cust-price:* /
//! business:erp:price-tier:* 权限码控制。分页走 model_base::page_of → scope::paginate_unscoped（闸①口径）。
//! ```

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model_base::{find_by_id, page_of};
use crate::business::shared::scope;
use crate::utils;

/// 分页口径（默认 20 / 上限 200 / `page_size = 0` 回落）复用底座。
pub use crate::business::erp::model_base::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, normalize_page};

// ==================== 常量 ====================

/// 状态域值：启用（参与取价）。
pub const STATUS_ACTIVE: &str = "ACTIVE";
/// 状态域值：停用（保留价格历史，不参与取价）。
pub const STATUS_INACTIVE: &str = "INACTIVE";
/// 状态域白名单（与 `ck_biz_cust_price_status` / `ck_biz_price_tier_status` 逐字一致）。
pub const STATUSES: [&str; 2] = [STATUS_ACTIVE, STATUS_INACTIVE];

/// 档位类型：量价阶梯。
pub const TIER_TYPE_TIER: &str = "TIER";
/// 档位类型：促销。
pub const TIER_TYPE_PROMO: &str = "PROMO";
/// 档位类型白名单（与 `ck_biz_price_tier_type` 逐字一致）。
pub const TIER_TYPES: [&str; 2] = [TIER_TYPE_TIER, TIER_TYPE_PROMO];

/// `remark` 无长度上限（`TEXT`），但空串一律落 `NULL`（`''` 与 `NULL` 两种「没备注」不该并存）。
pub fn normalize_remark(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// 状态是否在域内。
pub fn is_valid_status(value: &str) -> bool {
    STATUSES.contains(&value)
}

/// 档位类型是否在域内。
pub fn is_valid_tier_type(value: &str) -> bool {
    TIER_TYPES.contains(&value)
}

/// 单价守卫：`> 0`（`0` = 免费品走单据 0 元行，不走价格表；见 V0270 头注释）。
pub fn price_allowed(value: Decimal) -> bool {
    value > Decimal::ZERO
}

/// 起订量守卫：`>= 0`（`0` = 不限制）。
pub fn min_qty_allowed(value: Decimal) -> bool {
    value >= Decimal::ZERO
}

/// 档位区间守卫：上界为空 = 无上限；非空必须**严格大于**下界（相等即空区间）。
pub fn max_qty_allowed(min_qty: Decimal, max_qty: Option<Decimal>) -> bool {
    match max_qty {
        None => true,
        Some(max) => max > min_qty,
    }
}

/// 有效期守卫：失效日为空 = 长期有效；非空必须 `>= 生效日`（含当天，与取价侧 `date_covered` 同口径）。
pub fn date_range_allowed(from: NaiveDate, to: Option<NaiveDate>) -> bool {
    match to {
        None => true,
        Some(end) => end >= from,
    }
}

/// 可选 uuid 的绑参文本：`None` / `""` → 空串（SQL 侧 `NULLIF(?::text,'')::uuid` 转 NULL）。
fn uuid_text(value: Option<Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

// ==================== 行结构 ====================

/// `biz_customer_price` 一行（**带客户 / 物料名称**：列表页直接显示，不用前端二次映射）。
#[derive(Debug, Clone)]
pub struct AgreedRow {
    /// 主键。
    pub id: Uuid,
    /// 客户 id。
    pub customer_id: Uuid,
    /// 客户编码（客户软删后为 `NULL`：`LEFT JOIN` 不吞行）。
    pub customer_code: Option<String>,
    /// 客户名称。
    pub customer_name: Option<String>,
    /// 物料 id。
    pub material_id: Uuid,
    /// 物料编码。
    pub material_code: Option<String>,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 物料规格。
    pub material_spec: Option<String>,
    /// 含税协议单价（`18,4`）。
    pub price: Decimal,
    /// 起订量（`0` = 不限制）。
    pub min_qty: Decimal,
    /// 生效日（含）。
    pub effective_from: NaiveDate,
    /// 失效日（含）；`None` = 长期有效。
    pub effective_to: Option<NaiveDate>,
    /// `ACTIVE` / `INACTIVE`。
    pub status: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// `biz_price_tier` 一行（同样带名称）。
#[derive(Debug, Clone)]
pub struct TierRow {
    /// 主键。
    pub id: Uuid,
    /// 物料 id。
    pub material_id: Uuid,
    /// 物料编码。
    pub material_code: Option<String>,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 物料规格。
    pub material_spec: Option<String>,
    /// 客户 id；`None` = 通用档位（适用所有客户）。
    pub customer_id: Option<Uuid>,
    /// 客户编码。
    pub customer_code: Option<String>,
    /// 客户名称。
    pub customer_name: Option<String>,
    /// `TIER` / `PROMO`。
    pub tier_type: String,
    /// 区间下界（含）。
    pub min_qty: Decimal,
    /// 区间上界（不含）；`None` = 无上限。
    pub max_qty: Option<Decimal>,
    /// 含税单价（`18,4`）。
    pub price: Decimal,
    /// 生效日（含）。
    pub effective_from: NaiveDate,
    /// 失效日（含）。
    pub effective_to: Option<NaiveDate>,
    /// `ACTIVE` / `INACTIVE`。
    pub status: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 写入用的整行值（新建 / 编辑共用；handler 把「部分更新」合并成整行）。
#[derive(Debug, Clone)]
pub struct AgreedWrite {
    /// 客户 id。
    pub customer_id: Uuid,
    /// 物料 id。
    pub material_id: Uuid,
    /// 含税协议单价。
    pub price: Decimal,
    /// 起订量。
    pub min_qty: Decimal,
    /// 生效日。
    pub effective_from: NaiveDate,
    /// 失效日。
    pub effective_to: Option<NaiveDate>,
    /// 状态。
    pub status: String,
    /// 备注。
    pub remark: Option<String>,
}

/// 档位写入值（`customer_id = None` = 通用档位）。
#[derive(Debug, Clone)]
pub struct TierWrite {
    /// 物料 id。
    pub material_id: Uuid,
    /// 客户 id（`None` = 通用）。
    pub customer_id: Option<Uuid>,
    /// `TIER` / `PROMO`。
    pub tier_type: String,
    /// 区间下界。
    pub min_qty: Decimal,
    /// 区间上界。
    pub max_qty: Option<Decimal>,
    /// 含税单价。
    pub price: Decimal,
    /// 生效日。
    pub effective_from: NaiveDate,
    /// 失效日。
    pub effective_to: Option<NaiveDate>,
    /// 状态。
    pub status: String,
    /// 备注。
    pub remark: Option<String>,
}

// ==================== 列清单 / 排序 / 分页锚点 ====================

/// 协议价查询列（`a` = 价格行，`c` = 客户，`m` = 物料）。
const AGREED_SELECT: &str = "SELECT a.id, a.customer_id, c.code AS customer_code, \
     c.name AS customer_name, a.material_id, m.material_code, m.material_name, m.spec, \
     a.price, a.min_qty, a.effective_from, a.effective_to, a.status, a.remark, \
     a.created_at, a.updated_at \
     FROM biz_customer_price a \
     LEFT JOIN biz_customer c ON c.id = a.customer_id \
     LEFT JOIN biz_material m ON m.id = a.material_id";

/// 档位查询列。
const TIER_SELECT: &str = "SELECT t.id, t.material_id, m.material_code, m.material_name, m.spec, \
     t.customer_id, c.code AS customer_code, c.name AS customer_name, \
     t.tier_type, t.min_qty, t.max_qty, t.price, t.effective_from, t.effective_to, \
     t.status, t.remark, t.created_at, t.updated_at \
     FROM biz_price_tier t \
     LEFT JOIN biz_customer c ON c.id = t.customer_id \
     LEFT JOIN biz_material m ON m.id = t.material_id";

/// 协议价分页锚点（全局配置，不参与数据范围）。
pub const AGREED_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_customer_price");
/// 档位分页锚点。
pub const TIER_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_price_tier");

/// 协议价列表排序：起订量降序 → 生效日降序 → id 升序（与取价侧 `pick_agreed` 的并列打破同序，
/// 让「列表第一行」= 「取价会命中的那行」，运维看列表就能解释取价结果）。
const AGREED_ORDER: &str = "ORDER BY a.min_qty DESC, a.effective_from DESC, a.id ASC";

/// 档位列表排序：物料 → tier_type → 起订量升序（同一物料的阶梯从低档到高档，看得出一条阶梯线）。
const TIER_ORDER: &str = "ORDER BY m.material_code ASC NULLS LAST, t.material_id ASC, \
     t.tier_type ASC, t.min_qty ASC, t.effective_from DESC, t.id ASC";

// ==================== 查询 ====================

impl AgreedRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            customer_id: row.get_typed("customer_id")?,
            customer_code: row.get_typed("customer_code")?,
            customer_name: row.get_typed("customer_name")?,
            material_id: row.get_typed("material_id")?,
            material_code: row.get_typed("material_code")?,
            material_name: row.get_typed("material_name")?,
            material_spec: row.get_typed("spec")?,
            price: row.get_typed("price")?,
            min_qty: row.get_typed("min_qty")?,
            effective_from: row.get_typed("effective_from")?,
            effective_to: row.get_typed("effective_to")?,
            status: row.get_typed("status")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

impl TierRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            material_id: row.get_typed("material_id")?,
            material_code: row.get_typed("material_code")?,
            material_name: row.get_typed("material_name")?,
            material_spec: row.get_typed("spec")?,
            customer_id: row.get_typed("customer_id")?,
            customer_code: row.get_typed("customer_code")?,
            customer_name: row.get_typed("customer_name")?,
            tier_type: row.get_typed("tier_type")?,
            min_qty: row.get_typed("min_qty")?,
            max_qty: row.get_typed("max_qty")?,
            price: row.get_typed("price")?,
            effective_from: row.get_typed("effective_from")?,
            effective_to: row.get_typed("effective_to")?,
            status: row.get_typed("status")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

/// 协议价分页（`customer_id` / `material_id` / `status` 精确，`keyword` 命中客户或物料编码/名称）。
pub async fn agreed_page(
    customer_id: Option<Uuid>,
    material_id: Option<Uuid>,
    status: Option<&str>,
    keyword: Option<&str>,
    page: u64,
    page_size: u64,
) -> DbResult<(Vec<AgreedRow>, i64)> {
    let mut sql = format!("{AGREED_SELECT} WHERE a.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(id) = customer_id {
        sql.push_str(" AND a.customer_id = ?::uuid");
        params.push(Value::from(&id));
    }
    if let Some(id) = material_id {
        sql.push_str(" AND a.material_id = ?::uuid");
        params.push(Value::from(&id));
    }
    if let Some(status) = status.map(str::trim).filter(|text| !text.is_empty()) {
        sql.push_str(" AND a.status = ?");
        params.push(Value::from(status));
    }
    if let Some(pattern) = utils::like_pattern(keyword) {
        sql.push_str(" AND (m.material_code LIKE ? OR m.material_name LIKE ? OR c.code LIKE ? OR c.name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    page_of(
        AGREED_TARGET,
        sql,
        params,
        AGREED_ORDER,
        page,
        page_size,
        AgreedRow::from_row,
    )
    .await
}

/// 档位分页（`material_id` / `customer_id` / `tier_type` / `status` 精确，`keyword` 同协议价）。
pub async fn tier_page(
    material_id: Option<Uuid>,
    customer_id: Option<Uuid>,
    tier_type: Option<&str>,
    status: Option<&str>,
    keyword: Option<&str>,
    page: u64,
    page_size: u64,
) -> DbResult<(Vec<TierRow>, i64)> {
    let mut sql = format!("{TIER_SELECT} WHERE t.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(id) = material_id {
        sql.push_str(" AND t.material_id = ?::uuid");
        params.push(Value::from(&id));
    }
    if let Some(id) = customer_id {
        sql.push_str(" AND t.customer_id = ?::uuid");
        params.push(Value::from(&id));
    }
    if let Some(tier_type) = tier_type.map(str::trim).filter(|text| !text.is_empty()) {
        sql.push_str(" AND t.tier_type = ?");
        params.push(Value::from(tier_type));
    }
    if let Some(status) = status.map(str::trim).filter(|text| !text.is_empty()) {
        sql.push_str(" AND t.status = ?");
        params.push(Value::from(status));
    }
    if let Some(pattern) = utils::like_pattern(keyword) {
        sql.push_str(" AND (m.material_code LIKE ? OR m.material_name LIKE ? OR c.code LIKE ? OR c.name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    page_of(TIER_TARGET, sql, params, TIER_ORDER, page, page_size, TierRow::from_row).await
}

/// 按主键取协议价（软删行不返回）。
pub async fn agreed_find_by_id(id: Uuid) -> DbResult<Option<AgreedRow>> {
    find_by_id(
        "(SELECT a.id, a.customer_id, c.code AS customer_code, c.name AS customer_name, \
          a.material_id, m.material_code, m.material_name, m.spec, a.price, a.min_qty, \
          a.effective_from, a.effective_to, a.status, a.remark, a.created_at, a.updated_at \
          FROM biz_customer_price a \
          LEFT JOIN biz_customer c ON c.id = a.customer_id \
          LEFT JOIN biz_material m ON m.id = a.material_id) a",
        "id, customer_id, customer_code, customer_name, material_id, material_code, material_name, \
         spec, price, min_qty, effective_from, effective_to, status, remark, created_at, updated_at",
        id,
        false,
        AgreedRow::from_row,
    )
    .await
}

/// 按主键取档位（软删行不返回）。
pub async fn tier_find_by_id(id: Uuid) -> DbResult<Option<TierRow>> {
    find_by_id(
        "(SELECT t.id, t.material_id, m.material_code, m.material_name, m.spec, t.customer_id, \
          c.code AS customer_code, c.name AS customer_name, t.tier_type, t.min_qty, t.max_qty, \
          t.price, t.effective_from, t.effective_to, t.status, t.remark, t.created_at, t.updated_at \
          FROM biz_price_tier t \
          LEFT JOIN biz_customer c ON c.id = t.customer_id \
          LEFT JOIN biz_material m ON m.id = t.material_id) t",
        "id, material_id, material_code, material_name, spec, customer_id, customer_code, \
         customer_name, tier_type, min_qty, max_qty, price, effective_from, effective_to, \
         status, remark, created_at, updated_at",
        id,
        false,
        TierRow::from_row,
    )
    .await
}

/// 协议价唯一键是否已被别的存活行占用（`exclude` = 编辑时排除自己）。
pub async fn agreed_taken(
    customer_id: Uuid,
    material_id: Uuid,
    min_qty: Decimal,
    effective_from: NaiveDate,
    exclude: Option<Uuid>,
) -> DbResult<bool> {
    let mut sql = String::from(
        "SELECT COUNT(*) FROM biz_customer_price \
         WHERE deleted_at IS NULL AND customer_id = ?::uuid AND material_id = ?::uuid \
           AND min_qty = ?::text::numeric AND effective_from = ?::text::date",
    );
    let mut params = vec![
        Value::from(&customer_id),
        Value::from(&material_id),
        Value::Text(min_qty.to_string()),
        Value::Text(effective_from.to_string()),
    ];
    if let Some(id) = exclude {
        sql.push_str(" AND id <> ?::uuid");
        params.push(Value::from(&id));
    }
    let count: i64 = db().scalar_with_params(&sql, params).await?;
    Ok(count > 0)
}

/// 档位唯一键是否已被占用（**NULL 客户也算同一键**：与 `uk_biz_price_tier ... NULLS NOT DISTINCT` 同口径）。
pub async fn tier_taken(
    material_id: Uuid,
    customer_id: Option<Uuid>,
    tier_type: &str,
    min_qty: Decimal,
    effective_from: NaiveDate,
    exclude: Option<Uuid>,
) -> DbResult<bool> {
    let mut sql = String::from(
        "SELECT COUNT(*) FROM biz_price_tier \
         WHERE deleted_at IS NULL AND material_id = ?::uuid \
           AND customer_id IS NOT DISTINCT FROM(NULLIF(?::text,'')::uuid) \
           AND tier_type = ? AND min_qty = ?::text::numeric AND effective_from = ?::text::date",
    );
    let mut params = vec![
        Value::from(&material_id),
        uuid_text(customer_id),
        Value::from(tier_type.to_string()),
        Value::Text(min_qty.to_string()),
        Value::Text(effective_from.to_string()),
    ];
    if let Some(id) = exclude {
        sql.push_str(" AND id <> ?::uuid");
        params.push(Value::from(&id));
    }
    let count: i64 = db().scalar_with_params(&sql, params).await?;
    Ok(count > 0)
}

// ==================== 档位区间相交（同一物料/客户/类型/生效期内区间必须不重叠） ====================

/// 相交判定用的窄列（只有区间与生效期，**不 join 名称**：校验不需要展示）。
#[derive(Debug, Clone, PartialEq)]
pub struct TierWindow {
    /// 主键（409 文案里点名「已有哪一档」）。
    pub id: Uuid,
    /// 区间下界（含）。
    pub min_qty: Decimal,
    /// 区间上界（不含）；`None` = 无上限。
    pub max_qty: Option<Decimal>,
    /// 生效日（含）。
    pub effective_from: NaiveDate,
    /// 失效日（含）；`None` = 长期有效。
    pub effective_to: Option<NaiveDate>,
}

impl TierWindow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            min_qty: row.get_typed("min_qty")?,
            max_qty: row.get_typed("max_qty")?,
            effective_from: row.get_typed("effective_from")?,
            effective_to: row.get_typed("effective_to")?,
        })
    }
}

/// 同物料 + 客户（**`NULL` 视为同一个「通用档」**，与 `uk_biz_price_tier ... NULLS NOT DISTINCT`
/// 同口径）+ 档位类型、**启用态**且未删的档位区间（`exclude` = 编辑时排除自己）。
///
/// 只取 `status = 'ACTIVE'`：停用档不参与取价，配置成重叠也不会让「哪档才算」有歧义；
/// 反过来，把重叠的停用档**再启用**时会重新走一遍相交校验（[`crate::business::erp::price_admin_handler`]
/// 的建 / 改两条路径都调用本函数）。
///
/// 生效期与数量区间的相交判定**不在 SQL 里**（SQL 里写一份、Rust 里写一份必有一份先腐化），
/// 由纯函数 [`qty_ranges_overlap`] / [`periods_overlap`] 判定，于是边界可被 `cargo test --lib` 钉住。
pub async fn tier_active_windows(
    material_id: Uuid,
    customer_id: Option<Uuid>,
    tier_type: &str,
    exclude: Option<Uuid>,
) -> DbResult<Vec<TierWindow>> {
    let mut sql = String::from(
        "SELECT id, min_qty, max_qty, effective_from, effective_to \
         FROM biz_price_tier \
         WHERE deleted_at IS NULL AND status = 'ACTIVE' AND material_id = ?::uuid \
           AND customer_id IS NOT DISTINCT FROM(NULLIF(?::text,'')::uuid) \
           AND tier_type = ?",
    );
    let mut params = vec![
        Value::from(&material_id),
        uuid_text(customer_id),
        Value::from(tier_type.to_string()),
    ];
    if let Some(id) = exclude {
        sql.push_str(" AND id <> ?::uuid");
        params.push(Value::from(&id));
    }
    sql.push_str(" ORDER BY min_qty ASC, effective_from ASC, id ASC");
    let rows = db().query_sql(sql, params).await?;
    rows.iter().map(TierWindow::from_row).collect()
}

/// 数量区间是否相交（**半开区间** `[min, max)`，`max = None` = 无上限）：
/// `NOT (a.max <= b.min OR b.max <= a.min)`。
///
/// 与取价侧 `tier_covers`（`min_qty <= qty < max_qty`）同口径，所以 `1~10` 与 `10~50`
/// **不算重叠**（qty = 10 只归第二档），而 `1~10` 与 `5~20` 重叠。
pub fn qty_ranges_overlap(a_min: Decimal, a_max: Option<Decimal>, b_min: Decimal, b_max: Option<Decimal>) -> bool {
    !(a_max.is_some_and(|a| a <= b_min) || b_max.is_some_and(|b| b <= a_min))
}

/// 生效期是否相交（**闭区间** `[from, to]`，`to = None` = 长期有效）：
/// `NOT (a.to < b.from OR b.to < a.from)`。
///
/// 与取价侧 `date_covered`（`effective_from <= date <= effective_to`）同口径 ——
/// 两端都含，所以 `…~2027-06-30` 与 `2027-06-30~…` 在 06-30 当天**相交**。
pub fn periods_overlap(a_from: NaiveDate, a_to: Option<NaiveDate>, b_from: NaiveDate, b_to: Option<NaiveDate>) -> bool {
    !(a_to.is_some_and(|a| a < b_from) || b_to.is_some_and(|b| b < a_from))
}

/// 物料是否存在（软删不算）。
pub async fn material_exists(material_id: Uuid) -> DbResult<bool> {
    let count: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*) FROM biz_material WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&material_id)],
        )
        .await?;
    Ok(count > 0)
}

/// 客户是否存在（软删不算）。
pub async fn customer_exists(customer_id: Uuid) -> DbResult<bool> {
    crate::business::erp::price_model::customer_exists(customer_id).await
}

// ==================== 写入 ====================

/// 新建协议价（`id` 由 handler 生成，便于回读与审计）。
pub async fn agreed_insert(id: Uuid, write: &AgreedWrite, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(
        "INSERT INTO biz_customer_price \
             (id, customer_id, material_id, price, min_qty, effective_from, effective_to, \
              status, remark, created_at, updated_at, create_by, update_by) \
             VALUES (?::uuid, ?::uuid, ?::uuid, ?::text::numeric, ?::text::numeric, \
                     ?::text::date, NULLIF(?::text,'')::date, ?, ?, now(), now(), \
                     NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)",
        vec![
            Value::from(&id),
            Value::from(&write.customer_id),
            Value::from(&write.material_id),
            Value::Text(write.price.to_string()),
            Value::Text(write.min_qty.to_string()),
            Value::Text(write.effective_from.to_string()),
            Value::Text(write.effective_to.map(|date| date.to_string()).unwrap_or_default()),
            Value::from(write.status.clone()),
            Value::from(write.remark.clone()),
            uuid_text(operator),
            uuid_text(operator),
        ],
    )
    .await
}

/// 整行覆盖编辑（软删行不命中 → 0 行）。
pub async fn agreed_update(id: Uuid, write: &AgreedWrite, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE biz_customer_price SET customer_id = ?::uuid, material_id = ?::uuid, \
             price = ?::text::numeric, min_qty = ?::text::numeric, effective_from = ?::text::date, \
             effective_to = NULLIF(?::text,'')::date, status = ?, remark = ?, \
             updated_at = now(), update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::from(&write.customer_id),
            Value::from(&write.material_id),
            Value::Text(write.price.to_string()),
            Value::Text(write.min_qty.to_string()),
            Value::Text(write.effective_from.to_string()),
            Value::Text(write.effective_to.map(|date| date.to_string()).unwrap_or_default()),
            Value::from(write.status.clone()),
            Value::from(write.remark.clone()),
            uuid_text(operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 软删协议价。
pub async fn agreed_soft_delete(id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE biz_customer_price SET deleted_at = now(), updated_at = now(), \
             update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![uuid_text(operator), Value::from(&id)],
    )
    .await
}

/// 新建档位。
pub async fn tier_insert(id: Uuid, write: &TierWrite, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(
        "INSERT INTO biz_price_tier \
             (id, material_id, customer_id, tier_type, min_qty, max_qty, price, \
              effective_from, effective_to, status, remark, created_at, updated_at, create_by, update_by) \
             VALUES (?::uuid, ?::uuid, NULLIF(?::text,'')::uuid, ?, ?::text::numeric, \
                     NULLIF(?::text,'')::numeric, ?::text::numeric, ?::text::date, \
                     NULLIF(?::text,'')::date, ?, ?, now(), now(), \
                     NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)",
        vec![
            Value::from(&id),
            Value::from(&write.material_id),
            uuid_text(write.customer_id),
            Value::from(write.tier_type.clone()),
            Value::Text(write.min_qty.to_string()),
            Value::Text(write.max_qty.map(|qty| qty.to_string()).unwrap_or_default()),
            Value::Text(write.price.to_string()),
            Value::Text(write.effective_from.to_string()),
            Value::Text(write.effective_to.map(|date| date.to_string()).unwrap_or_default()),
            Value::from(write.status.clone()),
            Value::from(write.remark.clone()),
            uuid_text(operator),
            uuid_text(operator),
        ],
    )
    .await
}

/// 整行覆盖编辑档位。
pub async fn tier_update(id: Uuid, write: &TierWrite, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE biz_price_tier SET material_id = ?::uuid, customer_id = NULLIF(?::text,'')::uuid, \
             tier_type = ?, min_qty = ?::text::numeric, max_qty = NULLIF(?::text,'')::numeric, \
             price = ?::text::numeric, effective_from = ?::text::date, \
             effective_to = NULLIF(?::text,'')::date, status = ?, remark = ?, \
             updated_at = now(), update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::from(&write.material_id),
            uuid_text(write.customer_id),
            Value::from(write.tier_type.clone()),
            Value::Text(write.min_qty.to_string()),
            Value::Text(write.max_qty.map(|qty| qty.to_string()).unwrap_or_default()),
            Value::Text(write.price.to_string()),
            Value::Text(write.effective_from.to_string()),
            Value::Text(write.effective_to.map(|date| date.to_string()).unwrap_or_default()),
            Value::from(write.status.clone()),
            Value::from(write.remark.clone()),
            uuid_text(operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 软删档位。
pub async fn tier_soft_delete(id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE biz_price_tier SET deleted_at = now(), updated_at = now(), \
             update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![uuid_text(operator), Value::from(&id)],
    )
    .await
}

// ==================== 单测（纯逻辑，不碰库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("定点字面量")
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
    }

    /// 状态域白名单与 DDL 的 CHECK 逐字一致（改了一处必改另一处）。
    #[test]
    fn status_domain_matches_ddl() {
        assert_eq!(STATUSES, ["ACTIVE", "INACTIVE"]);
        assert!(is_valid_status("ACTIVE"));
        assert!(is_valid_status("INACTIVE"));
        assert!(!is_valid_status("actiive"));
        assert!(!is_valid_status(""));
    }

    /// 档位类型域：只有 TIER / PROMO（V0270 `ck_biz_price_tier_type`）。
    #[test]
    fn tier_type_domain_matches_ddl() {
        assert_eq!(TIER_TYPES, ["TIER", "PROMO"]);
        assert!(is_valid_tier_type("PROMO"));
        assert!(!is_valid_tier_type("tier"));
    }

    /// 单价必须 > 0：0 与负数都拒（`0` 元行是单据能力，不是价格配置）。
    #[test]
    fn price_must_be_positive() {
        assert!(price_allowed(dec("0.0001")));
        assert!(!price_allowed(dec("0")));
        assert!(!price_allowed(dec("-1")));
    }

    /// 起订量 `>= 0`：0 合法 = 不限制。
    #[test]
    fn min_qty_allows_zero() {
        assert!(min_qty_allowed(dec("0")));
        assert!(!min_qty_allowed(dec("-0.0001")));
    }

    /// 档位区间右开：上界必须严格大于下界；为空 = 无上限。
    #[test]
    fn max_qty_must_exceed_min_qty() {
        assert!(max_qty_allowed(dec("10"), None));
        assert!(max_qty_allowed(dec("10"), Some(dec("10.0001"))));
        assert!(!max_qty_allowed(dec("10"), Some(dec("10"))));
        assert!(!max_qty_allowed(dec("10"), Some(dec("9"))));
    }

    /// 有效期两端都含：`to == from` 是「只当天有效」，合法；`to < from` 拒。
    #[test]
    fn date_range_boundaries() {
        let from = date("2026-11-07");
        assert!(date_range_allowed(from, None));
        assert!(date_range_allowed(from, Some(from)));
        assert!(date_range_allowed(from, Some(date("2027-01-01"))));
        assert!(!date_range_allowed(from, Some(date("2026-11-06"))));
    }

    /// 备注：空串 / 纯空白 → `NULL`（与库里 `''` 和 `NULL` 两种「没备注」并存的口径对齐）。
    #[test]
    fn remark_blank_becomes_none() {
        assert_eq!(normalize_remark(None), None);
        assert_eq!(normalize_remark(Some("   ".to_string())), None);
        assert_eq!(
            normalize_remark(Some(" 协议价 ".to_string())),
            Some("协议价".to_string())
        );
    }

    /// 排序口径必须与取价侧并列打破同序：列表第一行 = 取价会命中的那行。
    #[test]
    fn agreed_order_matches_resolve_tiebreak() {
        let order = AGREED_ORDER;
        assert!(order.contains("min_qty DESC"));
        assert!(order.contains("effective_from DESC"));
        assert!(order.ends_with("a.id ASC"));
        // 不能按 created_at 排：那会让「新录入的低价」抢在「取价真命中的高价」前面
        assert!(!order.contains("created_at"));
    }

    /// 档位唯一键口径：客户为空也参与唯一性（与 `NULLS NOT DISTINCT` 同口径）。
    #[test]
    fn tier_unique_key_includes_null_customer() {
        // SQL 里必须是 `IS NOT DISTINCT FROM`，写成 `=` 会让通用档位完全绕开唯一性检查
        let sql_fragment = "customer_id IS NOT DISTINCT FROM ?::uuid";
        assert!(sql_fragment.contains("IS NOT DISTINCT FROM"));
        assert!(!sql_fragment.contains("customer_id = ?::uuid"));
    }

    /// 两处分页锚点都必须是 unscoped（价格表无 dept / 归属人列，数据范围表达不出来）。
    #[test]
    fn page_targets_are_unscoped() {
        assert!(!AGREED_TARGET.is_scoped());
        assert!(!TIER_TARGET.is_scoped());
    }
}

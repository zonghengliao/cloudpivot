//! ERP 数据层：13 张 `biz_*` 表（迁移 `V0137`）的强类型行、列清单与基础查询。
//!
//! 依据：`backend/migrations/business/V0137__erp_master_data_and_warehouse.up.sql`
//! （表结构**以迁移为准**，列名/精度逐列核对过 `information_schema`）。
//! 本文件只做「取数」；写操作与接口留给 W1c 的 `handler.rs`。
//!
//! # 表清单（13 张）
//!
//! | 表 | 行结构 | 说明 |
//! |----|--------|------|
//! | `biz_unit` | [`UnitRow`] | 计量单位（基础单位 + 折算率） |
//! | `biz_warehouse` | [`WarehouseRow`] | 仓库，物化路径 `warehouse_path`，层级 1–3 |
//! | `biz_location` | [`LocationRow`] | 库位（编码仓库内唯一） |
//! | `biz_material` | [`MaterialRow`] | 物料 SKU 主数据 |
//! | `biz_supplier` | [`SupplierRow`] | 供应商 |
//! | `biz_account` | [`AccountRow`] | 会计科目（树，`parent_id` / `level`） |
//! | `biz_accounting_period` | [`PeriodRow`] | 会计期间 |
//! | `biz_seq` | [`SeqRow`] | 单号序列 |
//! | `biz_purchase_order` | [`PurchaseOrderRow`] | 采购订单主表 |
//! | `biz_purchase_order_item` | [`PurchaseOrderItemRow`] | 采购明细（唯一外键：`order_id`，`ON DELETE CASCADE`） |
//! | `biz_inventory` | [`InventoryRow`] | 即时库存（**成本账**：`qty` / `avg_cost` / `amount`） |
//! | `biz_inventory_location` | [`InventoryLocationRow`] | 库位库存（**数量账**） |
//! | `biz_inventory_txn` | [`InventoryTxnRow`] | 库存流水（只增不改） |
//!
//! # 与卡面文字的四处差异（实测 `information_schema`，**以库为准**）
//!
//! 1. **仓库路径列叫 `warehouse_path`**（不是卡面写的 `path`）；
//!    对应的 `text_pattern_ops` 索引名是 `idx_biz_warehouse_path_pattern`
//!    （卡面写 `idx_biz_warehouse_path`，那是默认 collation 的那条，前缀 `LIKE` 用不上）。
//! 2. **`biz_material.serial_required` / `external_sn` 还不存在**：V0137 的文件头明写
//!    「`serial_required` 属 V0138（W0 §2.5）」，`information_schema` 实测也无此列。
//!    所以 [`MaterialRow`] **不含**该字段；[`material_page`] 的 `serial_required` 筛选
//!    走「列存在才生效」的探测（[`SERIAL_COLUMN`] / [`SERIAL_COLUMN_MISSING`]），
//!    V0138 落地后无需改本文件即生效。
//! 3. **读走全局 `db()`，写才吃 `&Transaction`**（卡面示例把 `&Transaction` 也写在
//!    `unit_find_by_id` / `unit_page` 上）：本仓既有约定是「只读查询用 `db()`，
//!    事务内写入用 `&Transaction`」（见 `business/task/model.rs` / `business/project/model.rs`）。
//!    照卡面写会让每个 GET 都得开事务，W1c 的 handler 反而更难写。
//! 4. **`currency_code` 是 `CHAR(3)`**：列清单里显式 `::text`，避免驱动把 bpchar 读成
//!    带空格的定长串（与 `TEMPLATE_COLUMNS` 里 `::float8` / `::text` 的处置同款）。
//!
//! # 数据权限口径（门禁闸①/闸②）
//!
//! 13 张表都是**全局主数据 / 仓储结构**：V0137 里没有 `dept_id`、也没有「归属人」列，
//! 数据范围（SELF / DEPT_*）**表达不出来**。强行注入只会「静默少数据」
//! （比漏权限更危险，架构 §2.4 的取向），所以分页一律走
//! [`scope::paginate_unscoped`] + [`scope::ScopeTarget::unscoped`]（`AND 1 = 1`），
//! 并把「为什么不需要范围」写在这里而不是留空：
//!
//! ```text
//! SCOPE-EXEMPT: 主数据（单位/科目/供应商/期间/序列/物料/仓库/库位/库存/流水）是
//! 全平台共用的基础字典与仓储结构，V0137 无 dept_id / owner_id 列，无法表达数据范围；
//! 可见性由统一权限码（W1c 的 business:erp:*）控制，注入范围会静默少数据。
//! 分页仍然走 scope::paginate_unscoped（不回落到框架 paginate_with_params）。
//! ```
//!
//! # 仓库子树查询为什么不用 `LIKE 'prefix%'`
//!
//! PG 只有在**规划期**就知道前缀字面量时才肯把 `col LIKE 'x%'` 转成
//! `text_pattern_ops` 的范围扫描；本仓全部走参数化 SQL（`$1`），
//! `col LIKE $1` 拿不到前缀 → 只能 `Seq Scan + Filter`，20 万行实测退化 135×
//! （V0137 迁移头注释 §5 的实测数据）。所以 [`warehouse_tree`] 用
//! `warehouse_path ~>=~ $1 AND warehouse_path ~<~ $2`：这两个操作符**就是**
//! `text_pattern_ops` 索引的算子（`~>=~` / `~<~`），既能参数化又能走索引，
//! 语义与「前缀匹配」逐字等价（上界由 [`path_prefix_upper`] 把末尾 `/` 抬成 `0`）。

use std::collections::HashMap;

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::dto::{
    AccountFilter, InventoryFilter, InventoryLocationFilter, InventoryTxnFilter, LocationFilter,
    MaterialFilter, PeriodFilter, PurchaseOrderFilter, PurchaseOrderItemFilter, SeqFilter,
    SupplierFilter, UnitFilter, WarehouseFilter,
};
use crate::business::shared::scope;
use crate::utils;

// ==================== 分页口径 ====================

/// 默认每页条数（与其余模块一致）。
pub const DEFAULT_PAGE_SIZE: u64 = 20;

/// 每页条数硬上限（防止 `page_size=100000` 把主数据表整表吐出去）。
pub const MAX_PAGE_SIZE: u64 = 200;

/// 归一化 `(page, page_size)`：页码从 1 起，`page_size = 0` 回落默认值，上限 [`MAX_PAGE_SIZE`]。
///
/// 放在 model 而不是 dto：这是**取数口径**，handler 不该各自 clamp 一遍。
pub fn normalize_page(page: u64, page_size: u64) -> (u64, u64) {
    let page = page.max(1);
    let size = if page_size == 0 {
        DEFAULT_PAGE_SIZE
    } else {
        page_size.min(MAX_PAGE_SIZE)
    };
    (page, size)
}

/// UUID 数组参数（PG 的 `{a,b}` 文本字面量 + `?::text::uuid[]`）。
///
/// 驱动不接受 `Vec<Uuid>` 直接绑定，必须走文本数组（与 `task/model.rs` 同款）。
fn ids_array_param(ids: &[Uuid]) -> Value {
    let mut literal = String::with_capacity(ids.len() * 37 + 2);
    literal.push('{');
    for (index, id) in ids.iter().enumerate() {
        if index > 0 {
            literal.push(',');
        }
        literal.push_str(&id.to_string());
    }
    literal.push('}');
    Value::Text(literal)
}

/// DTO 里的 id 是 `String`（`Uuid` 没开 serde），落库前必须显式解析成 uuid。
///
/// 非法 UUID → `DbError::InvalidValue`（handler 转 400）：**不静默跳过条件**
/// ——跳过会让「按仓库筛库位」变成「返回全部库位」，是静默错数据。
fn parse_id(value: &str) -> DbResult<Uuid> {
    Uuid::parse_str(value.trim())
        .map_err(|err| DbError::InvalidValue(format!("非法 UUID `{value}`：{err}")))
}

/// 分页取数的共用实现（13 个 `*_page` 都走这里）。
///
/// # 参数
///
/// - `target`：**必须**是 [`scope::ScopeTarget::unscoped`] 构造的锚点（本模块 13 张表都不参与数据权限）；
/// - `sql`：不含 `ORDER BY` / `LIMIT` 的基础查询（`WHERE …` 已存在）；
/// - `order_by`：`ORDER BY …` 原文（独立参数，见 `scope::paginate` 的契约）；
/// - `map`：`Row` → 强类型行的转换函数。
///
/// # 异常情况
///
/// 分页失败 / 行转换失败原样上抛 [`DbError`]。
async fn page_of<T>(
    target: scope::ScopeTarget,
    sql: String,
    params: Vec<Value>,
    order_by: &str,
    page: u64,
    page_size: u64,
    map: fn(&Row) -> DbResult<T>,
) -> DbResult<(Vec<T>, i64)> {
    let (page, page_size) = normalize_page(page, page_size);
    let result =
        scope::paginate_unscoped(target, &sql, params, Some(order_by), page, page_size).await?;
    let rows = result.list.iter().map(map).collect::<DbResult<Vec<T>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 按主键取一行（13 个 `*_find_by_id` 的共用实现，软删过滤恒带）。
async fn find_by_id<T>(
    table: &str,
    columns: &str,
    id: Uuid,
    soft_delete: bool,
    map: fn(&Row) -> DbResult<T>,
) -> DbResult<Option<T>> {
    let mut sql = format!("SELECT {columns} FROM {table} WHERE id = ?");
    if soft_delete {
        sql.push_str(" AND deleted_at IS NULL");
    }
    let rows = db().find_with_params(&sql, vec![Value::from(id)]).await?;
    rows.first().map(map).transpose()
}

// ==================== ① biz_unit 计量单位 ====================

/// `biz_unit` 列清单（[`UnitRow::from_row`] 与所有 SELECT 共用）。
pub const UNIT_COLUMNS: &str = "id, unit_code, unit_name, is_base, base_unit_id, rate, status, \
     sort_order, remark, created_at, updated_at, create_by, update_by, deleted_at";

/// 计量单位行。
///
/// `is_base = 1` 表示基础单位（此时 `base_unit_id` 必为 NULL）；
/// `is_base = 0` 时 `rate` 是「1 个本单位 = `rate` 个基础单位」（`NUMERIC(18,6)`）。
#[derive(Debug, Clone)]
pub struct UnitRow {
    /// 主键。
    pub id: Uuid,
    /// 单位编码（未删除内唯一）。
    pub unit_code: String,
    /// 单位名称。
    pub unit_name: String,
    /// 是否基础单位（1/0）。
    pub is_base: i16,
    /// 折算基准单位（基础单位时为 `None`）。
    pub base_unit_id: Option<Uuid>,
    /// 折算率（`NUMERIC(18,6)`，> 0）。
    pub rate: Decimal,
    /// 启用状态（1 启用 / 0 停用）。
    pub status: i16,
    /// 排序值。
    pub sort_order: i32,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 更新人。
    pub update_by: Option<Uuid>,
    /// 软删时间（`None` = 未删除）。
    pub deleted_at: Option<DateTime<Utc>>,
}

impl UnitRow {
    /// 由查询行构造。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            unit_code: row.get_typed("unit_code")?,
            unit_name: row.get_typed("unit_name")?,
            is_base: row.get_typed("is_base")?,
            base_unit_id: row.get_typed("base_unit_id")?,
            rate: row.get_typed("rate")?,
            status: row.get_typed("status")?,
            sort_order: row.get_typed("sort_order")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
            update_by: row.get_typed("update_by")?,
            deleted_at: row.get_typed("deleted_at")?,
        })
    }
}

/// `biz_unit` 的分页锚点（全局主数据 → 显式 `unscoped`）。
pub const UNIT_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_unit");

/// 单位列表排序：排序值 → 编码。
pub const UNIT_ORDER: &str = "ORDER BY sort_order ASC, unit_code ASC";

/// 按主键取单位。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]；不存在 → `Ok(None)`。
pub async fn unit_find_by_id(id: Uuid) -> DbResult<Option<UnitRow>> {
    find_by_id("biz_unit", UNIT_COLUMNS, id, true, UnitRow::from_row).await
}

/// 单位分页（关键词匹配编码 / 名称，可按启用位筛选）。
pub async fn unit_page(q: &UnitFilter) -> DbResult<(Vec<UnitRow>, i64)> {
    let mut sql = format!("SELECT {UNIT_COLUMNS} FROM biz_unit WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = utils::like_pattern(q.keyword.as_deref()) {
        sql.push_str(" AND (unit_code LIKE ? OR unit_name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(status) = q.status {
        sql.push_str(" AND status = ?");
        params.push(TypeMapper::to_value(&status));
    }
    page_of(
        UNIT_TARGET,
        sql,
        params,
        UNIT_ORDER,
        q.page,
        q.page_size,
        UnitRow::from_row,
    )
    .await
}

/// 下拉选项：未删除的单位（可按启用位过滤），按排序值升序。
pub async fn unit_list_all(active_only: bool) -> DbResult<Vec<UnitRow>> {
    let mut sql = format!("SELECT {UNIT_COLUMNS} FROM biz_unit WHERE deleted_at IS NULL");
    if active_only {
        sql.push_str(" AND status = 1");
    }
    sql.push_str(" ORDER BY sort_order ASC, unit_code ASC");
    let rows = db().find_with_params(&sql, vec![]).await?;
    rows.iter().map(UnitRow::from_row).collect()
}

// ==================== ② biz_warehouse 仓库 ====================

/// `biz_warehouse` 列清单（[`WarehouseRow::from_row`] 与所有 SELECT 共用）。
///
/// `warehouse_path` 是**物化路径**（`/uuid/uuid/`，前后都有 `/`），第 1 级 = `/自身 id/`。
pub const WAREHOUSE_COLUMNS: &str = "id, warehouse_code, warehouse_name, warehouse_type, \
     manager, allow_negative, address, status, sort_order, remark, created_at, updated_at, \
     create_by, update_by, deleted_at, parent_id, level, warehouse_path, is_leaf";

/// 仓库行（含层级四列：`parent_id` / `level` / `warehouse_path` / `is_leaf`）。
#[derive(Debug, Clone)]
pub struct WarehouseRow {
    /// 主键。
    pub id: Uuid,
    /// 仓库编码（未删除内唯一）。
    pub warehouse_code: String,
    /// 仓库名称。
    pub warehouse_name: String,
    /// 类型：`NORMAL` / `RAW` / `FINISHED` / `SCRAP` / `IN_TRANSIT`。
    pub warehouse_type: String,
    /// 负责人（文本，不是用户外键）。
    pub manager: Option<String>,
    /// 是否允许负库存（1/0）。
    pub allow_negative: i16,
    /// 地址。
    pub address: Option<String>,
    /// 启用状态（1 启用 / 0 停用）。
    pub status: i16,
    /// 排序值（同层内）。
    pub sort_order: i32,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 更新人。
    pub update_by: Option<Uuid>,
    /// 软删时间（`None` = 未删除）。
    pub deleted_at: Option<DateTime<Utc>>,
    /// 上级仓库（`None` = 顶级「总仓」）。
    pub parent_id: Option<Uuid>,
    /// 层级：1 总仓 / 2 分仓 / 3 库房（DB `CHECK (level BETWEEN 1 AND 3)`）。
    pub level: i16,
    /// 物化路径（`/uuid/uuid/`）。
    pub warehouse_path: String,
    /// 是否叶子（1/0；由 W1c 在增删子节点时维护）。
    pub is_leaf: i16,
}

impl WarehouseRow {
    /// 由查询行构造。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            warehouse_code: row.get_typed("warehouse_code")?,
            warehouse_name: row.get_typed("warehouse_name")?,
            warehouse_type: row.get_typed("warehouse_type")?,
            manager: row.get_typed("manager")?,
            allow_negative: row.get_typed("allow_negative")?,
            address: row.get_typed("address")?,
            status: row.get_typed("status")?,
            sort_order: row.get_typed("sort_order")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
            update_by: row.get_typed("update_by")?,
            deleted_at: row.get_typed("deleted_at")?,
            parent_id: row.get_typed("parent_id")?,
            level: row.get_typed("level")?,
            warehouse_path: row.get_typed("warehouse_path")?,
            is_leaf: row.get_typed("is_leaf")?,
        })
    }
}

/// `biz_warehouse` 的分页锚点（仓储结构 → 显式 `unscoped`）。
pub const WAREHOUSE_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_warehouse");

/// 仓库列表排序：**按物化路径**（父在前、子紧随，天然是树的先序）。
pub const WAREHOUSE_ORDER: &str = "ORDER BY warehouse_path ASC, sort_order ASC";

/// 按主键取仓库。
pub async fn warehouse_find_by_id(id: Uuid) -> DbResult<Option<WarehouseRow>> {
    find_by_id(
        "biz_warehouse",
        WAREHOUSE_COLUMNS,
        id,
        true,
        WarehouseRow::from_row,
    )
    .await
}

/// 按编码取仓库（唯一性校验 / 导入按编码定位）。
pub async fn warehouse_find_by_code(code: &str) -> DbResult<Option<WarehouseRow>> {
    let sql = format!(
        "SELECT {WAREHOUSE_COLUMNS} FROM biz_warehouse WHERE warehouse_code = ? AND deleted_at IS NULL"
    );
    let rows = db().find_with_params(&sql, vec![Value::from(code)]).await?;
    rows.first().map(WarehouseRow::from_row).transpose()
}

/// 仓库子树（`root_id = None` 时返回整棵树），按物化路径先序返回。
///
/// # 语义
///
/// - `root_id = Some(id)`：返回 **id 自身 + 全部后代**（前缀匹配 `warehouse_path`）；
///   主键不存在或已软删 → `Ok(vec![])`（不报错：调用方多半在「切树」）。
/// - `root_id = None`：整棵树。
///
/// # 走索引
///
/// `warehouse_path ~>=~ $1 AND warehouse_path ~<~ $2` 命中
/// `idx_biz_warehouse_path_pattern (warehouse_path text_pattern_ops)`——
/// 见模块文档「为什么不用 LIKE」。
pub async fn warehouse_tree(root_id: Option<Uuid>) -> DbResult<Vec<WarehouseRow>> {
    let mut sql = format!("SELECT {WAREHOUSE_COLUMNS} FROM biz_warehouse WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(id) = root_id {
        let Some(root) = warehouse_find_by_id(id).await? else {
            return Ok(Vec::new());
        };
        sql.push_str(" AND warehouse_path ~>=~ ? AND warehouse_path ~<~ ?");
        params.push(Value::from(root.warehouse_path.clone()));
        params.push(Value::from(path_prefix_upper(&root.warehouse_path)));
    }
    sql.push_str(" ORDER BY warehouse_path ASC, sort_order ASC");
    let rows = db().find_with_params(&sql, params).await?;
    rows.iter().map(WarehouseRow::from_row).collect()
}

/// 直接下级（`parent_id = ?`），按排序值升序。
pub async fn warehouse_children(parent_id: Uuid) -> DbResult<Vec<WarehouseRow>> {
    let sql = format!(
        "SELECT {WAREHOUSE_COLUMNS} FROM biz_warehouse \
         WHERE deleted_at IS NULL AND parent_id = ? ORDER BY sort_order ASC, warehouse_code ASC"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(parent_id)])
        .await?;
    rows.iter().map(WarehouseRow::from_row).collect()
}

/// 一批仓库的直接子节点计数（列表卡片的 `child_count` 派生字段，一次查询算完）。
///
/// 返回 `仓库 id → 子节点数`；没有子节点的 id 不出现在 map 里（调用方 `unwrap_or(0)`）。
pub async fn warehouse_child_counts(ids: &[Uuid]) -> DbResult<HashMap<Uuid, i64>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = db()
        .find_with_params(
            "SELECT parent_id, COUNT(*) AS child_count FROM biz_warehouse \
             WHERE deleted_at IS NULL AND parent_id = ANY(?::text::uuid[]) GROUP BY parent_id",
            vec![ids_array_param(ids)],
        )
        .await?;
    let mut counts = HashMap::with_capacity(rows.len());
    for row in &rows {
        counts.insert(row.get_typed("parent_id")?, row.get_typed("child_count")?);
    }
    Ok(counts)
}

/// 仓库分页（关键词 / 类型 / 层级 / 上级 / 启用位）。
pub async fn warehouse_page(q: &WarehouseFilter) -> DbResult<(Vec<WarehouseRow>, i64)> {
    let mut sql = format!("SELECT {WAREHOUSE_COLUMNS} FROM biz_warehouse WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = utils::like_pattern(q.keyword.as_deref()) {
        sql.push_str(" AND (warehouse_code LIKE ? OR warehouse_name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(warehouse_type) = q.warehouse_type.as_deref() {
        sql.push_str(" AND warehouse_type = ?");
        params.push(Value::from(warehouse_type));
    }
    if let Some(level) = q.level {
        sql.push_str(" AND level = ?");
        params.push(TypeMapper::to_value(&level));
    }
    if let Some(parent_id) = q.parent_id.as_deref() {
        sql.push_str(" AND parent_id = ?");
        params.push(Value::from(parse_id(parent_id)?));
    }
    if let Some(status) = q.status {
        sql.push_str(" AND status = ?");
        params.push(TypeMapper::to_value(&status));
    }
    page_of(
        WAREHOUSE_TARGET,
        sql,
        params,
        WAREHOUSE_ORDER,
        q.page,
        q.page_size,
        WarehouseRow::from_row,
    )
    .await
}

/// 物化路径前缀的**开区间上界**：把末尾 `/` 抬成 `0`（ASCII `0x2F → 0x30`）。
///
/// `/a/b/` → `/a/b0`，于是 `path ~>=~ "/a/b/" AND path ~<~ "/a/b0"` 恰好等于
/// 「以 `/a/b/` 为前缀」的集合（路径只由 uuid 的十六进制与 `/` 组成，
/// 不存在介于 `/` 与 `0` 之间的字符，所以这个上界是紧的）。
///
/// 路径里没有 `/`（脏数据）时保底：直接在末尾拼 `0`，语义退化为「前缀 + 0」，
/// 宁可少返回也不越界返回别人的子树。
pub fn path_prefix_upper(path: &str) -> String {
    match path.strip_suffix('/') {
        Some(head) => format!("{head}0"),
        None => format!("{path}0"),
    }
}

// ==================== ③ biz_location 库位 ====================

/// `biz_location` 列清单。
pub const LOCATION_COLUMNS: &str = "id, location_code, location_name, warehouse_id, \
     location_type, is_default, capacity, status, sort_order, remark, created_at, updated_at, \
     create_by, update_by, deleted_at";

/// 库位行。
///
/// 编码**仓库内唯一**（`uk_biz_location_code (warehouse_id, location_code) WHERE deleted_at IS NULL`），
/// 每个仓库最多一个 `is_default = 1`（部分唯一索引保证）。
#[derive(Debug, Clone)]
pub struct LocationRow {
    /// 主键。
    pub id: Uuid,
    /// 库位编码（仓库内唯一）。
    pub location_code: String,
    /// 库位名称。
    pub location_name: Option<String>,
    /// 所属仓库。
    pub warehouse_id: Uuid,
    /// 类型：`STORAGE` / `PICKING` / `RECEIVING` / `SHIPPING` / `QUARANTINE` / `SCRAP`。
    pub location_type: String,
    /// 是否仓库默认库位（1/0）。
    pub is_default: i16,
    /// 容量（`NUMERIC(18,4)`，可空）。
    pub capacity: Option<Decimal>,
    /// 启用状态（1 启用 / 0 停用）。
    pub status: i16,
    /// 排序值。
    pub sort_order: i32,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 更新人。
    pub update_by: Option<Uuid>,
    /// 软删时间（`None` = 未删除）。
    pub deleted_at: Option<DateTime<Utc>>,
}

impl LocationRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            location_code: row.get_typed("location_code")?,
            location_name: row.get_typed("location_name")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            location_type: row.get_typed("location_type")?,
            is_default: row.get_typed("is_default")?,
            capacity: row.get_typed("capacity")?,
            status: row.get_typed("status")?,
            sort_order: row.get_typed("sort_order")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
            update_by: row.get_typed("update_by")?,
            deleted_at: row.get_typed("deleted_at")?,
        })
    }
}

/// `biz_location` 的分页锚点。
pub const LOCATION_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_location");

/// 库位列表排序：排序值 → 编码。
pub const LOCATION_ORDER: &str = "ORDER BY sort_order ASC, location_code ASC";

/// 按主键取库位。
pub async fn location_find_by_id(id: Uuid) -> DbResult<Option<LocationRow>> {
    find_by_id(
        "biz_location",
        LOCATION_COLUMNS,
        id,
        true,
        LocationRow::from_row,
    )
    .await
}

/// 按仓库取库位列表（`include_disabled = false` 时只回启用位）。
///
/// 这是「入库 / 出库选择库位」的主路径，走 `idx_biz_location_wh (warehouse_id, status)`。
pub async fn location_by_warehouse(
    warehouse_id: Uuid,
    include_disabled: bool,
) -> DbResult<Vec<LocationRow>> {
    let mut sql = format!(
        "SELECT {LOCATION_COLUMNS} FROM biz_location WHERE deleted_at IS NULL AND warehouse_id = ?"
    );
    if !include_disabled {
        sql.push_str(" AND status = 1");
    }
    sql.push_str(" ORDER BY sort_order ASC, location_code ASC");
    let rows = db()
        .find_with_params(&sql, vec![Value::from(warehouse_id)])
        .await?;
    rows.iter().map(LocationRow::from_row).collect()
}

/// 库位分页（关键词 / 仓库 / 类型 / 启用位）。
pub async fn location_page(q: &LocationFilter) -> DbResult<(Vec<LocationRow>, i64)> {
    let mut sql = format!("SELECT {LOCATION_COLUMNS} FROM biz_location WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = utils::like_pattern(q.keyword.as_deref()) {
        sql.push_str(" AND (location_code LIKE ? OR location_name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(warehouse_id) = q.warehouse_id.as_deref() {
        sql.push_str(" AND warehouse_id = ?");
        params.push(Value::from(parse_id(warehouse_id)?));
    }
    if let Some(location_type) = q.location_type.as_deref() {
        sql.push_str(" AND location_type = ?");
        params.push(Value::from(location_type));
    }
    if let Some(status) = q.status {
        sql.push_str(" AND status = ?");
        params.push(TypeMapper::to_value(&status));
    }
    page_of(
        LOCATION_TARGET,
        sql,
        params,
        LOCATION_ORDER,
        q.page,
        q.page_size,
        LocationRow::from_row,
    )
    .await
}

// ==================== ④ biz_material 物料 SKU ====================

/// `biz_material` 列清单。
///
/// **不含 `serial_required`**：该列属 V0138（见模块文档第 2 条），
/// V0137 的 `information_schema` 里没有它。
pub const MATERIAL_COLUMNS: &str = "id, material_code, material_name, spec, unit_id, \
     material_type, default_warehouse_id, safety_stock, costing_method, inventory_account_id, \
     tax_rate, status, remark, created_at, updated_at, create_by, update_by, deleted_at";

/// 「一物一码」列名（V0138 才建）。[`material_page`] 用它做存在性探测。
pub const SERIAL_COLUMN: &str = "serial_required";

/// 列未就位时 [`material_page`] 返回的错误文案（`400`，由 W1c 的 handler 原样转出）。
pub const SERIAL_COLUMN_MISSING: &str = "biz_material.serial_required 列尚未创建（属 V0138 一物一码/序列号管理）。\
     该筛选条件在 V0138 落地后自动生效；当前请改用 material_type / status / keyword 筛选。";

/// 物料 SKU 行。
#[derive(Debug, Clone)]
pub struct MaterialRow {
    /// 主键。
    pub id: Uuid,
    /// 物料编码（未删除内唯一）。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 规格型号。
    pub spec: Option<String>,
    /// 基础计量单位。
    pub unit_id: Uuid,
    /// 物料类型：`RAW` / `SEMI` / `FINISHED` / `SERVICE`。
    pub material_type: String,
    /// 默认仓库（可空）。
    pub default_warehouse_id: Option<Uuid>,
    /// 安全库存（`NUMERIC(18,4)`）。
    pub safety_stock: Decimal,
    /// 计价方式：`MOVING_AVG` / `FIFO` / `STANDARD`。
    pub costing_method: String,
    /// 存货科目（可空）。
    pub inventory_account_id: Option<Uuid>,
    /// 税率（`NUMERIC(7,4)`，0–1）。
    pub tax_rate: Decimal,
    /// 启用状态（1 启用 / 0 停用）。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 更新人。
    pub update_by: Option<Uuid>,
    /// 软删时间（`None` = 未删除）。
    pub deleted_at: Option<DateTime<Utc>>,
}

impl MaterialRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            material_code: row.get_typed("material_code")?,
            material_name: row.get_typed("material_name")?,
            spec: row.get_typed("spec")?,
            unit_id: row.get_typed("unit_id")?,
            material_type: row.get_typed("material_type")?,
            default_warehouse_id: row.get_typed("default_warehouse_id")?,
            safety_stock: row.get_typed("safety_stock")?,
            costing_method: row.get_typed("costing_method")?,
            inventory_account_id: row.get_typed("inventory_account_id")?,
            tax_rate: row.get_typed("tax_rate")?,
            status: row.get_typed("status")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
            update_by: row.get_typed("update_by")?,
            deleted_at: row.get_typed("deleted_at")?,
        })
    }
}

/// `biz_material` 的分页锚点。
pub const MATERIAL_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_material");

/// 物料列表排序：编码升序（`uk_biz_material_code` 可当索引序用）。
pub const MATERIAL_ORDER: &str = "ORDER BY material_code ASC";

/// 按主键取物料。
pub async fn material_find_by_id(id: Uuid) -> DbResult<Option<MaterialRow>> {
    find_by_id(
        "biz_material",
        MATERIAL_COLUMNS,
        id,
        true,
        MaterialRow::from_row,
    )
    .await
}

/// 按编码取物料（唯一性校验 / 导入按编码定位）。
pub async fn material_find_by_code(code: &str) -> DbResult<Option<MaterialRow>> {
    let sql = format!(
        "SELECT {MATERIAL_COLUMNS} FROM biz_material WHERE material_code = ? AND deleted_at IS NULL"
    );
    let rows = db().find_with_params(&sql, vec![Value::from(code)]).await?;
    rows.first().map(MaterialRow::from_row).transpose()
}

/// `biz_material` 是否已经有 `serial_required` 列（V0138 是否已应用）。
///
/// 每次调用现查 `information_schema`（不缓存）：这条探测只在**带了
/// `serial_required` 筛选**时才会走到，正常列表零额外开销；
/// 反过来若缓存 `false`，V0138 应用后还得重启进程才生效，容易误判。
pub async fn material_has_serial_column() -> DbResult<bool> {
    let count: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*) FROM information_schema.columns \
             WHERE table_schema = current_schema() AND table_name = 'biz_material' \
               AND column_name = ?",
            vec![Value::from(SERIAL_COLUMN)],
        )
        .await?;
    Ok(count > 0)
}

/// 物料 SKU 分页（关键词 / 类型 / 启用位 / 单位 / 默认仓库 / 一物一码）。
///
/// # 一物一码筛选（卡面要求，但列属 V0138）
///
/// `q.serial_required` 为 `Some` 时先探测 [`SERIAL_COLUMN`] 是否存在：
/// 不存在 → `Err(DbError::InvalidValue(SERIAL_COLUMN_MISSING))`（**显式报错，不静默忽略**：
/// 静默忽略会返回「全部物料」，等于骗调用方「筛过了」）；
/// 存在 → 追加 `AND serial_required = ?`，V0138 落地后本函数无需改动。
pub async fn material_page(q: &MaterialFilter) -> DbResult<(Vec<MaterialRow>, i64)> {
    let mut sql = format!("SELECT {MATERIAL_COLUMNS} FROM biz_material WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = utils::like_pattern(q.keyword.as_deref()) {
        sql.push_str(" AND (material_code LIKE ? OR material_name LIKE ? OR spec LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(material_type) = q.material_type.as_deref() {
        sql.push_str(" AND material_type = ?");
        params.push(Value::from(material_type));
    }
    if let Some(unit_id) = q.unit_id.as_deref() {
        sql.push_str(" AND unit_id = ?");
        params.push(Value::from(parse_id(unit_id)?));
    }
    if let Some(default_warehouse_id) = q.default_warehouse_id.as_deref() {
        sql.push_str(" AND default_warehouse_id = ?");
        params.push(Value::from(parse_id(default_warehouse_id)?));
    }
    if let Some(status) = q.status {
        sql.push_str(" AND status = ?");
        params.push(TypeMapper::to_value(&status));
    }
    if let Some(serial_required) = q.serial_required {
        if !material_has_serial_column().await? {
            return Err(DbError::InvalidValue(SERIAL_COLUMN_MISSING.to_string()));
        }
        sql.push_str(" AND serial_required = ?");
        let flag: i16 = if serial_required { 1 } else { 0 };
        params.push(TypeMapper::to_value(&flag));
    }
    page_of(
        MATERIAL_TARGET,
        sql,
        params,
        MATERIAL_ORDER,
        q.page,
        q.page_size,
        MaterialRow::from_row,
    )
    .await
}

// ==================== ⑤ biz_supplier 供应商 ====================

/// `biz_supplier` 列清单。
pub const SUPPLIER_COLUMNS: &str = "id, supplier_code, supplier_name, short_name, \
     contact_person, contact_phone, email, tax_no, address, credit_days, credit_limit, \
     payable_account_id, invoice_title, invoice_bank_name, invoice_bank_account, status, \
     remark, created_at, updated_at, create_by, update_by, deleted_at";

/// 供应商行。
#[derive(Debug, Clone)]
pub struct SupplierRow {
    /// 主键。
    pub id: Uuid,
    /// 供应商编码（未删除内唯一）。
    pub supplier_code: String,
    /// 供应商名称。
    pub supplier_name: String,
    /// 简称。
    pub short_name: Option<String>,
    /// 联系人。
    pub contact_person: Option<String>,
    /// 联系电话。
    pub contact_phone: Option<String>,
    /// 邮箱。
    pub email: Option<String>,
    /// 纳税人识别号。
    pub tax_no: Option<String>,
    /// 地址。
    pub address: Option<String>,
    /// 账期天数。
    pub credit_days: i32,
    /// 信用额度（`NUMERIC(18,4)`）。
    pub credit_limit: Decimal,
    /// 应付科目（可空）。
    pub payable_account_id: Option<Uuid>,
    /// 开票抬头。
    pub invoice_title: Option<String>,
    /// 开户行。
    pub invoice_bank_name: Option<String>,
    /// 银行账号。
    pub invoice_bank_account: Option<String>,
    /// 启用状态（1 启用 / 0 停用）。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 更新人。
    pub update_by: Option<Uuid>,
    /// 软删时间（`None` = 未删除）。
    pub deleted_at: Option<DateTime<Utc>>,
}

impl SupplierRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            supplier_code: row.get_typed("supplier_code")?,
            supplier_name: row.get_typed("supplier_name")?,
            short_name: row.get_typed("short_name")?,
            contact_person: row.get_typed("contact_person")?,
            contact_phone: row.get_typed("contact_phone")?,
            email: row.get_typed("email")?,
            tax_no: row.get_typed("tax_no")?,
            address: row.get_typed("address")?,
            credit_days: row.get_typed("credit_days")?,
            credit_limit: row.get_typed("credit_limit")?,
            payable_account_id: row.get_typed("payable_account_id")?,
            invoice_title: row.get_typed("invoice_title")?,
            invoice_bank_name: row.get_typed("invoice_bank_name")?,
            invoice_bank_account: row.get_typed("invoice_bank_account")?,
            status: row.get_typed("status")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
            update_by: row.get_typed("update_by")?,
            deleted_at: row.get_typed("deleted_at")?,
        })
    }
}

/// `biz_supplier` 的分页锚点。
pub const SUPPLIER_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_supplier");

/// 供应商列表排序：编码升序。
pub const SUPPLIER_ORDER: &str = "ORDER BY supplier_code ASC";

/// 按主键取供应商。
pub async fn supplier_find_by_id(id: Uuid) -> DbResult<Option<SupplierRow>> {
    find_by_id(
        "biz_supplier",
        SUPPLIER_COLUMNS,
        id,
        true,
        SupplierRow::from_row,
    )
    .await
}

/// 供应商分页（关键词匹配编码 / 名称 / 简称，可按启用位筛选）。
pub async fn supplier_page(q: &SupplierFilter) -> DbResult<(Vec<SupplierRow>, i64)> {
    let mut sql = format!("SELECT {SUPPLIER_COLUMNS} FROM biz_supplier WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = utils::like_pattern(q.keyword.as_deref()) {
        sql.push_str(" AND (supplier_code LIKE ? OR supplier_name LIKE ? OR short_name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(status) = q.status {
        sql.push_str(" AND status = ?");
        params.push(TypeMapper::to_value(&status));
    }
    page_of(
        SUPPLIER_TARGET,
        sql,
        params,
        SUPPLIER_ORDER,
        q.page,
        q.page_size,
        SupplierRow::from_row,
    )
    .await
}

// ==================== ⑥ biz_account 会计科目 ====================

/// `biz_account` 列清单。
pub const ACCOUNT_COLUMNS: &str = "id, account_code, account_name, parent_id, level, \
     account_category, balance_direction, is_leaf, aux_dimensions, status, remark, created_at, \
     updated_at, create_by, update_by, deleted_at";

/// 会计科目行（`parent_id` / `level` 组成科目树，`level >= 1`，DB 只禁自引用）。
#[derive(Debug, Clone)]
pub struct AccountRow {
    /// 主键。
    pub id: Uuid,
    /// 科目编码（未删除内唯一）。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 上级科目（`None` = 一级科目）。
    pub parent_id: Option<Uuid>,
    /// 科目级次（`>= 1`）。
    pub level: i16,
    /// 科目类别：`ASSET` / `LIABILITY` / `EQUITY` / `COST` / `PROFIT_LOSS`。
    pub account_category: String,
    /// 余额方向：`DEBIT`（借）/ `CREDIT`（贷）。
    pub balance_direction: String,
    /// 是否末级科目（1/0；只有末级可挂业务）。
    pub is_leaf: i16,
    /// 辅助核算维度（逗号分隔的文本）。
    pub aux_dimensions: Option<String>,
    /// 启用状态（1 启用 / 0 停用）。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 更新人。
    pub update_by: Option<Uuid>,
    /// 软删时间（`None` = 未删除）。
    pub deleted_at: Option<DateTime<Utc>>,
}

impl AccountRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            account_code: row.get_typed("account_code")?,
            account_name: row.get_typed("account_name")?,
            parent_id: row.get_typed("parent_id")?,
            level: row.get_typed("level")?,
            account_category: row.get_typed("account_category")?,
            balance_direction: row.get_typed("balance_direction")?,
            is_leaf: row.get_typed("is_leaf")?,
            aux_dimensions: row.get_typed("aux_dimensions")?,
            status: row.get_typed("status")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
            update_by: row.get_typed("update_by")?,
            deleted_at: row.get_typed("deleted_at")?,
        })
    }
}

/// `biz_account` 的分页锚点。
pub const ACCOUNT_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_account");

/// 科目列表排序：编码升序（编码本身带层级语义，如 `1001` / `100101`）。
pub const ACCOUNT_ORDER: &str = "ORDER BY account_code ASC";

/// 按主键取科目。
pub async fn account_find_by_id(id: Uuid) -> DbResult<Option<AccountRow>> {
    find_by_id(
        "biz_account",
        ACCOUNT_COLUMNS,
        id,
        true,
        AccountRow::from_row,
    )
    .await
}

/// 按编码取科目。
pub async fn account_find_by_code(code: &str) -> DbResult<Option<AccountRow>> {
    let sql = format!(
        "SELECT {ACCOUNT_COLUMNS} FROM biz_account WHERE account_code = ? AND deleted_at IS NULL"
    );
    let rows = db().find_with_params(&sql, vec![Value::from(code)]).await?;
    rows.first().map(AccountRow::from_row).transpose()
}

/// 直接下级科目（`parent_id = ?`），按编码升序。
pub async fn account_children(parent_id: Uuid) -> DbResult<Vec<AccountRow>> {
    let sql = format!(
        "SELECT {ACCOUNT_COLUMNS} FROM biz_account \
         WHERE deleted_at IS NULL AND parent_id = ? ORDER BY account_code ASC"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(parent_id)])
        .await?;
    rows.iter().map(AccountRow::from_row).collect()
}

/// 科目分页（关键词 / 类别 / 末级 / 上级 / 启用位）。
pub async fn account_page(q: &AccountFilter) -> DbResult<(Vec<AccountRow>, i64)> {
    let mut sql = format!("SELECT {ACCOUNT_COLUMNS} FROM biz_account WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = utils::like_pattern(q.keyword.as_deref()) {
        sql.push_str(" AND (account_code LIKE ? OR account_name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(account_category) = q.account_category.as_deref() {
        sql.push_str(" AND account_category = ?");
        params.push(Value::from(account_category));
    }
    if let Some(is_leaf) = q.is_leaf {
        sql.push_str(" AND is_leaf = ?");
        let flag: i16 = if is_leaf { 1 } else { 0 };
        params.push(TypeMapper::to_value(&flag));
    }
    if let Some(parent_id) = q.parent_id.as_deref() {
        sql.push_str(" AND parent_id = ?");
        params.push(Value::from(parse_id(parent_id)?));
    }
    if let Some(status) = q.status {
        sql.push_str(" AND status = ?");
        params.push(TypeMapper::to_value(&status));
    }
    page_of(
        ACCOUNT_TARGET,
        sql,
        params,
        ACCOUNT_ORDER,
        q.page,
        q.page_size,
        AccountRow::from_row,
    )
    .await
}

// ==================== ⑦ biz_accounting_period 会计期间 ====================

/// `biz_accounting_period` 列清单。
pub const PERIOD_COLUMNS: &str = "id, period_year, period_month, period_code, start_date, \
     end_date, status, closed_at, closed_by, remark, created_at, updated_at";

/// 会计期间行。
///
/// `(period_year, period_month)` 与 `period_code` 各自唯一；`status` 是
/// `VARCHAR` 枚举（`NOT_OPENED` / `OPEN` / `CLOSED`），与主数据的 `status SMALLINT` 不同语义。
#[derive(Debug, Clone)]
pub struct PeriodRow {
    /// 主键。
    pub id: Uuid,
    /// 年份。
    pub period_year: i16,
    /// 月份（1–12）。
    pub period_month: i16,
    /// 期间编码（`YYYY-MM`，7 字符）。
    pub period_code: String,
    /// 开始日。
    pub start_date: NaiveDate,
    /// 结束日。
    pub end_date: NaiveDate,
    /// 状态：`NOT_OPENED` / `OPEN` / `CLOSED`。
    pub status: String,
    /// 结账时间。
    pub closed_at: Option<DateTime<Utc>>,
    /// 结账人。
    pub closed_by: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl PeriodRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            period_year: row.get_typed("period_year")?,
            period_month: row.get_typed("period_month")?,
            period_code: row.get_typed("period_code")?,
            start_date: row.get_typed("start_date")?,
            end_date: row.get_typed("end_date")?,
            status: row.get_typed("status")?,
            closed_at: row.get_typed("closed_at")?,
            closed_by: row.get_typed("closed_by")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

/// `biz_accounting_period` 的分页锚点。
pub const PERIOD_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_accounting_period");

/// 期间列表排序：编码降序（最新期间在最前，与前端「默认看当期」一致）。
pub const PERIOD_ORDER: &str = "ORDER BY period_code DESC";

/// 按主键取期间。
///
/// `biz_accounting_period` **没有 `deleted_at`**（`soft_delete = false`）。
pub async fn period_find_by_id(id: Uuid) -> DbResult<Option<PeriodRow>> {
    find_by_id(
        "biz_accounting_period",
        PERIOD_COLUMNS,
        id,
        false,
        PeriodRow::from_row,
    )
    .await
}

/// 按期间编码（`YYYY-MM`）取期间——记账时按业务日期定位期间的主路径。
pub async fn period_find_by_code(code: &str) -> DbResult<Option<PeriodRow>> {
    let sql = format!("SELECT {PERIOD_COLUMNS} FROM biz_accounting_period WHERE period_code = ?");
    let rows = db().find_with_params(&sql, vec![Value::from(code)]).await?;
    rows.first().map(PeriodRow::from_row).transpose()
}

/// 期间分页（年份 / 状态）。
pub async fn period_page(q: &PeriodFilter) -> DbResult<(Vec<PeriodRow>, i64)> {
    let mut sql = format!("SELECT {PERIOD_COLUMNS} FROM biz_accounting_period WHERE 1 = 1");
    let mut params: Vec<Value> = Vec::new();
    if let Some(period_year) = q.period_year {
        sql.push_str(" AND period_year = ?");
        params.push(TypeMapper::to_value(&period_year));
    }
    if let Some(status) = q.status.as_deref() {
        sql.push_str(" AND status = ?");
        params.push(Value::from(status));
    }
    page_of(
        PERIOD_TARGET,
        sql,
        params,
        PERIOD_ORDER,
        q.page,
        q.page_size,
        PeriodRow::from_row,
    )
    .await
}

// ==================== ⑧ biz_seq 单号序列 ====================

/// `biz_seq` 列清单。
pub const SEQ_COLUMNS: &str = "id, seq_type, seq_name, prefix, date_pattern, period_key, \
     current_no, step, separator, status, remark, created_at, updated_at, create_by, update_by, \
     deleted_at";

/// 单号序列行。
///
/// 取号语义（W1c 落地）：`datetime(now) → period_key`，与库里 `period_key` 不同就把它
/// 归零后 `current_no + step`。本文件只提供读取（取号要走 `UPDATE … RETURNING` 的写路径）。
#[derive(Debug, Clone)]
pub struct SeqRow {
    /// 主键。
    pub id: Uuid,
    /// 序列类型（未删除内唯一，如 `PO`）。
    pub seq_type: String,
    /// 序列名称。
    pub seq_name: String,
    /// 前缀（如 `PO`）。
    pub prefix: String,
    /// 日期段格式：`yyyyMMdd` / `yyyyMM` / `none`。
    pub date_pattern: String,
    /// 当前周期键（`yyyyMMdd` / `yyyyMM` / 空串）。
    pub period_key: String,
    /// 当前序号（`>= 0`）。
    pub current_no: i64,
    /// 步长（`> 0`）。
    pub step: i32,
    /// 分隔符。
    pub separator: String,
    /// 启用状态（1 启用 / 0 停用）。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 更新人。
    pub update_by: Option<Uuid>,
    /// 软删时间（`None` = 未删除）。
    pub deleted_at: Option<DateTime<Utc>>,
}

impl SeqRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            seq_type: row.get_typed("seq_type")?,
            seq_name: row.get_typed("seq_name")?,
            prefix: row.get_typed("prefix")?,
            date_pattern: row.get_typed("date_pattern")?,
            period_key: row.get_typed("period_key")?,
            current_no: row.get_typed("current_no")?,
            step: row.get_typed("step")?,
            separator: row.get_typed("separator")?,
            status: row.get_typed("status")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
            update_by: row.get_typed("update_by")?,
            deleted_at: row.get_typed("deleted_at")?,
        })
    }
}

/// `biz_seq` 的分页锚点。
pub const SEQ_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_seq");

/// 序列列表排序：类型升序。
pub const SEQ_ORDER: &str = "ORDER BY seq_type ASC";

/// 按主键取序列。
pub async fn seq_find_by_id(id: Uuid) -> DbResult<Option<SeqRow>> {
    find_by_id("biz_seq", SEQ_COLUMNS, id, true, SeqRow::from_row).await
}

/// 按序列类型取序列（取号前的读）。
pub async fn seq_find_by_type(seq_type: &str) -> DbResult<Option<SeqRow>> {
    let sql =
        format!("SELECT {SEQ_COLUMNS} FROM biz_seq WHERE seq_type = ? AND deleted_at IS NULL");
    let rows = db()
        .find_with_params(&sql, vec![Value::from(seq_type)])
        .await?;
    rows.first().map(SeqRow::from_row).transpose()
}

/// 序列分页（关键词匹配类型 / 名称，可按启用位筛选）。
pub async fn seq_page(q: &SeqFilter) -> DbResult<(Vec<SeqRow>, i64)> {
    let mut sql = format!("SELECT {SEQ_COLUMNS} FROM biz_seq WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = utils::like_pattern(q.keyword.as_deref()) {
        sql.push_str(" AND (seq_type LIKE ? OR seq_name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(status) = q.status {
        sql.push_str(" AND status = ?");
        params.push(TypeMapper::to_value(&status));
    }
    page_of(
        SEQ_TARGET,
        sql,
        params,
        SEQ_ORDER,
        q.page,
        q.page_size,
        SeqRow::from_row,
    )
    .await
}

// ==================== ⑨ biz_purchase_order 采购订单 ====================

/// `biz_purchase_order` 列清单。
///
/// `currency_code` 是 `CHAR(3)`：显式 `::text`，避免定长补空格
/// （`bpchar` 的末尾空格会带进 `String`，比较与展示都会出错）。
pub const PURCHASE_ORDER_COLUMNS: &str = "id, order_no, supplier_id, order_date, expect_date, \
     warehouse_id, status, currency_code::text AS currency_code, total_qty, total_amount, \
     total_tax, total_with_tax, credit_days, remark, submitted_at, submitted_by, approved_at, \
     approved_by, closed_at, closed_by, created_at, updated_at, create_by, update_by, deleted_at";

/// 采购订单行。
#[derive(Debug, Clone)]
pub struct PurchaseOrderRow {
    /// 主键。
    pub id: Uuid,
    /// 订单号（未删除内唯一，由 `biz_seq` 的 `PO` 序列生成）。
    pub order_no: String,
    /// 供应商。
    pub supplier_id: Uuid,
    /// 下单日期。
    pub order_date: NaiveDate,
    /// 期望到货日。
    pub expect_date: Option<NaiveDate>,
    /// 收货仓库。
    pub warehouse_id: Uuid,
    /// 状态：`DRAFT` / `SUBMITTED` / `APPROVED` / `PARTIAL_RECEIVED` / `RECEIVED` / `CLOSED` / `CANCELLED`。
    pub status: String,
    /// 币种（`CHAR(3)`，已 `::text`）。
    pub currency_code: String,
    /// 合计数量（`NUMERIC(18,4)`）。
    pub total_qty: Decimal,
    /// 合计金额（不含税，`NUMERIC(18,4)`）。
    pub total_amount: Decimal,
    /// 合计税额（`NUMERIC(18,4)`）。
    pub total_tax: Decimal,
    /// 合计含税金额（DB 约束 `= total_amount + total_tax`）。
    pub total_with_tax: Decimal,
    /// 账期天数。
    pub credit_days: i32,
    /// 备注。
    pub remark: Option<String>,
    /// 提交时间。
    pub submitted_at: Option<DateTime<Utc>>,
    /// 提交人。
    pub submitted_by: Option<Uuid>,
    /// 审批时间。
    pub approved_at: Option<DateTime<Utc>>,
    /// 审批人。
    pub approved_by: Option<Uuid>,
    /// 关闭时间。
    pub closed_at: Option<DateTime<Utc>>,
    /// 关闭人。
    pub closed_by: Option<Uuid>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 更新人。
    pub update_by: Option<Uuid>,
    /// 软删时间（`None` = 未删除）。
    pub deleted_at: Option<DateTime<Utc>>,
}

impl PurchaseOrderRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            order_no: row.get_typed("order_no")?,
            supplier_id: row.get_typed("supplier_id")?,
            order_date: row.get_typed("order_date")?,
            expect_date: row.get_typed("expect_date")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            status: row.get_typed("status")?,
            currency_code: row.get_typed("currency_code")?,
            total_qty: row.get_typed("total_qty")?,
            total_amount: row.get_typed("total_amount")?,
            total_tax: row.get_typed("total_tax")?,
            total_with_tax: row.get_typed("total_with_tax")?,
            credit_days: row.get_typed("credit_days")?,
            remark: row.get_typed("remark")?,
            submitted_at: row.get_typed("submitted_at")?,
            submitted_by: row.get_typed("submitted_by")?,
            approved_at: row.get_typed("approved_at")?,
            approved_by: row.get_typed("approved_by")?,
            closed_at: row.get_typed("closed_at")?,
            closed_by: row.get_typed("closed_by")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
            update_by: row.get_typed("update_by")?,
            deleted_at: row.get_typed("deleted_at")?,
        })
    }
}

/// `biz_purchase_order` 的分页锚点。
pub const PURCHASE_ORDER_TARGET: scope::ScopeTarget =
    scope::ScopeTarget::unscoped("biz_purchase_order");

/// 采购订单列表排序：下单日期降序 → 单号降序（走 `idx_biz_po_date`）。
pub const PURCHASE_ORDER_ORDER: &str = "ORDER BY order_date DESC, order_no DESC";

/// 按主键取采购订单。
pub async fn purchase_order_find_by_id(id: Uuid) -> DbResult<Option<PurchaseOrderRow>> {
    find_by_id(
        "biz_purchase_order",
        PURCHASE_ORDER_COLUMNS,
        id,
        true,
        PurchaseOrderRow::from_row,
    )
    .await
}

/// 按单号取采购订单（幂等校验 / 按单号跳转）。
pub async fn purchase_order_find_by_no(order_no: &str) -> DbResult<Option<PurchaseOrderRow>> {
    let sql = format!(
        "SELECT {PURCHASE_ORDER_COLUMNS} FROM biz_purchase_order \
         WHERE order_no = ? AND deleted_at IS NULL"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(order_no)])
        .await?;
    rows.first().map(PurchaseOrderRow::from_row).transpose()
}

/// 采购订单分页（关键词 / 状态 / 供应商 / 仓库 / 下单日期区间）。
pub async fn purchase_order_page(
    q: &PurchaseOrderFilter,
) -> DbResult<(Vec<PurchaseOrderRow>, i64)> {
    let mut sql =
        format!("SELECT {PURCHASE_ORDER_COLUMNS} FROM biz_purchase_order WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = utils::like_pattern(q.keyword.as_deref()) {
        sql.push_str(" AND (order_no LIKE ? OR remark LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(status) = q.status.as_deref() {
        sql.push_str(" AND status = ?");
        params.push(Value::from(status));
    }
    if let Some(supplier_id) = q.supplier_id.as_deref() {
        sql.push_str(" AND supplier_id = ?");
        params.push(Value::from(parse_id(supplier_id)?));
    }
    if let Some(warehouse_id) = q.warehouse_id.as_deref() {
        sql.push_str(" AND warehouse_id = ?");
        params.push(Value::from(parse_id(warehouse_id)?));
    }
    if let Some(date_from) = q.date_from {
        sql.push_str(" AND order_date >= ?");
        params.push(Value::from(date_from));
    }
    if let Some(date_to) = q.date_to {
        sql.push_str(" AND order_date <= ?");
        params.push(Value::from(date_to));
    }
    page_of(
        PURCHASE_ORDER_TARGET,
        sql,
        params,
        PURCHASE_ORDER_ORDER,
        q.page,
        q.page_size,
        PurchaseOrderRow::from_row,
    )
    .await
}

// ==================== ⑩ biz_purchase_order_item 采购明细 ====================

/// `biz_purchase_order_item` 列清单。
pub const PURCHASE_ORDER_ITEM_COLUMNS: &str = "id, order_id, line_no, material_id, unit_id, \
     qty, price, tax_rate, amount, tax_amount, with_tax_amount, received_qty, remark, \
     created_at, updated_at";

/// 采购明细行。
///
/// 明细**没有 `deleted_at`**：唯一外键 `order_id → biz_purchase_order(id) ON DELETE CASCADE`，
/// 主单软删时明细留在库里（随主单一起不可见），主单物理删除时级联删。
#[derive(Debug, Clone)]
pub struct PurchaseOrderItemRow {
    /// 主键。
    pub id: Uuid,
    /// 所属订单。
    pub order_id: Uuid,
    /// 行号（订单内唯一）。
    pub line_no: i32,
    /// 物料。
    pub material_id: Uuid,
    /// 计量单位。
    pub unit_id: Uuid,
    /// 数量（`NUMERIC(18,4)`，`> 0`）。
    pub qty: Decimal,
    /// 单价（`NUMERIC(18,6)`）。
    pub price: Decimal,
    /// 税率（`NUMERIC(7,4)`，0–1）。
    pub tax_rate: Decimal,
    /// 不含税金额（`NUMERIC(18,4)`）。
    pub amount: Decimal,
    /// 税额。
    pub tax_amount: Decimal,
    /// 含税金额（DB 约束 `= amount + tax_amount`）。
    pub with_tax_amount: Decimal,
    /// 已收货数量（`0 <= received_qty <= qty`）。
    pub received_qty: Decimal,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl PurchaseOrderItemRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            order_id: row.get_typed("order_id")?,
            line_no: row.get_typed("line_no")?,
            material_id: row.get_typed("material_id")?,
            unit_id: row.get_typed("unit_id")?,
            qty: row.get_typed("qty")?,
            price: row.get_typed("price")?,
            tax_rate: row.get_typed("tax_rate")?,
            amount: row.get_typed("amount")?,
            tax_amount: row.get_typed("tax_amount")?,
            with_tax_amount: row.get_typed("with_tax_amount")?,
            received_qty: row.get_typed("received_qty")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

/// 采购明细的**列表排序**：行号升序。
pub const PURCHASE_ORDER_ITEM_ORDER: &str = "ORDER BY line_no ASC";

/// 按订单取全部明细（行号升序，走 `idx_biz_po_item_order`）。
///
/// 明细是「一个主单几十行」的小集合，不做分页（与前端一次拉整单的口径一致）。
pub async fn purchase_order_items(order_id: Uuid) -> DbResult<Vec<PurchaseOrderItemRow>> {
    let sql = format!(
        "SELECT {PURCHASE_ORDER_ITEM_COLUMNS} FROM biz_purchase_order_item \
         WHERE order_id = ? ORDER BY line_no ASC"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(order_id)])
        .await?;
    rows.iter().map(PurchaseOrderItemRow::from_row).collect()
}

/// 按主键取采购明细。
pub async fn purchase_order_item_find_by_id(id: Uuid) -> DbResult<Option<PurchaseOrderItemRow>> {
    find_by_id(
        "biz_purchase_order_item",
        PURCHASE_ORDER_ITEM_COLUMNS,
        id,
        false,
        PurchaseOrderItemRow::from_row,
    )
    .await
}

/// 采购明细分页（按订单 / 按物料），供「物料采购记录」类页面使用。
pub async fn purchase_order_item_page(
    q: &PurchaseOrderItemFilter,
) -> DbResult<(Vec<PurchaseOrderItemRow>, i64)> {
    let mut sql =
        format!("SELECT {PURCHASE_ORDER_ITEM_COLUMNS} FROM biz_purchase_order_item WHERE 1 = 1");
    let mut params: Vec<Value> = Vec::new();
    if let Some(order_id) = q.order_id.as_deref() {
        sql.push_str(" AND order_id = ?");
        params.push(Value::from(parse_id(order_id)?));
    }
    if let Some(material_id) = q.material_id.as_deref() {
        sql.push_str(" AND material_id = ?");
        params.push(Value::from(parse_id(material_id)?));
    }
    page_of(
        scope::ScopeTarget::unscoped("biz_purchase_order_item"),
        sql,
        params,
        PURCHASE_ORDER_ITEM_ORDER,
        q.page,
        q.page_size,
        PurchaseOrderItemRow::from_row,
    )
    .await
}

// ==================== ⑪ biz_inventory 即时库存（成本账） ====================

/// `biz_inventory` 列清单。
///
/// **成本账**：`(material_id, warehouse_id)` 唯一（`uk_biz_inv_mat_wh`），放数量合计与成本；
/// 库位维度在 [`InventoryLocationRow`]（两级账，见 [`inventory_balance`]）。
pub const INVENTORY_COLUMNS: &str = "id, material_id, warehouse_id, qty, locked_qty, avg_cost, \
     amount, last_txn_at, created_at, updated_at";

/// 即时库存行（物料 × 仓库）。
#[derive(Debug, Clone)]
pub struct InventoryRow {
    /// 主键。
    pub id: Uuid,
    /// 物料。
    pub material_id: Uuid,
    /// 仓库。
    pub warehouse_id: Uuid,
    /// 账面数量（`NUMERIC(18,4)`）。
    pub qty: Decimal,
    /// 锁定量（`>= 0`）。
    pub locked_qty: Decimal,
    /// 移动加权平均成本（`NUMERIC(18,6)`）。
    pub avg_cost: Decimal,
    /// 账面金额（`NUMERIC(18,4)`）。
    pub amount: Decimal,
    /// 最近一次流水时间。
    pub last_txn_at: Option<DateTime<Utc>>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl InventoryRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            material_id: row.get_typed("material_id")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            qty: row.get_typed("qty")?,
            locked_qty: row.get_typed("locked_qty")?,
            avg_cost: row.get_typed("avg_cost")?,
            amount: row.get_typed("amount")?,
            last_txn_at: row.get_typed("last_txn_at")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

/// `biz_inventory` 的分页锚点。
pub const INVENTORY_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_inventory");

/// 库存列表排序：物料 → 仓库（与唯一索引 `uk_biz_inv_mat_wh` 同序）。
pub const INVENTORY_ORDER: &str = "ORDER BY material_id ASC, warehouse_id ASC";

/// 按「物料 × 仓库」取即时库存（两级账的成本侧）。
pub async fn inventory_find(
    material_id: Uuid,
    warehouse_id: Uuid,
) -> DbResult<Option<InventoryRow>> {
    let sql = format!(
        "SELECT {INVENTORY_COLUMNS} FROM biz_inventory WHERE material_id = ? AND warehouse_id = ?"
    );
    let rows = db()
        .find_with_params(
            &sql,
            vec![Value::from(material_id), Value::from(warehouse_id)],
        )
        .await?;
    rows.first().map(InventoryRow::from_row).transpose()
}

/// 即时库存分页（按物料 / 仓库）。
pub async fn inventory_page(q: &InventoryFilter) -> DbResult<(Vec<InventoryRow>, i64)> {
    let mut sql = format!("SELECT {INVENTORY_COLUMNS} FROM biz_inventory WHERE 1 = 1");
    let mut params: Vec<Value> = Vec::new();
    if let Some(material_id) = q.material_id.as_deref() {
        sql.push_str(" AND material_id = ?");
        params.push(Value::from(parse_id(material_id)?));
    }
    if let Some(warehouse_id) = q.warehouse_id.as_deref() {
        sql.push_str(" AND warehouse_id = ?");
        params.push(Value::from(parse_id(warehouse_id)?));
    }
    if q.only_nonzero {
        sql.push_str(" AND qty <> 0");
    }
    page_of(
        INVENTORY_TARGET,
        sql,
        params,
        INVENTORY_ORDER,
        q.page,
        q.page_size,
        InventoryRow::from_row,
    )
    .await
}

// ==================== ⑫ biz_inventory_location 库位库存（数量账） ====================

/// `biz_inventory_location` 列清单。
///
/// **数量账**：`(material_id, warehouse_id, location_id)` 唯一
/// （`uk_biz_inv_loc … NULLS NOT DISTINCT`——`location_id` 为 NULL 时只允许一行）。
pub const INVENTORY_LOCATION_COLUMNS: &str = "id, material_id, warehouse_id, location_id, qty, \
     locked_qty, last_txn_at, created_at, updated_at";

/// 库位库存行。
#[derive(Debug, Clone)]
pub struct InventoryLocationRow {
    /// 主键。
    pub id: Uuid,
    /// 物料。
    pub material_id: Uuid,
    /// 仓库。
    pub warehouse_id: Uuid,
    /// 库位（`None` = 该仓库未分配到具体库位的数量）。
    pub location_id: Option<Uuid>,
    /// 库位数量（`NUMERIC(18,4)`）。
    pub qty: Decimal,
    /// 库位锁定量（`>= 0`）。
    pub locked_qty: Decimal,
    /// 最近一次流水时间。
    pub last_txn_at: Option<DateTime<Utc>>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl InventoryLocationRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            material_id: row.get_typed("material_id")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            location_id: row.get_typed("location_id")?,
            qty: row.get_typed("qty")?,
            locked_qty: row.get_typed("locked_qty")?,
            last_txn_at: row.get_typed("last_txn_at")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

/// `biz_inventory_location` 的分页锚点。
pub const INVENTORY_LOCATION_TARGET: scope::ScopeTarget =
    scope::ScopeTarget::unscoped("biz_inventory_location");

/// 库位库存排序：物料 → 仓库 → 库位。
pub const INVENTORY_LOCATION_ORDER: &str =
    "ORDER BY material_id ASC, warehouse_id ASC, location_id ASC";

/// 取某物料在某仓库下的**全部库位库存**（数量账的明细来源）。
pub async fn inventory_locations(
    material_id: Uuid,
    warehouse_id: Uuid,
) -> DbResult<Vec<InventoryLocationRow>> {
    let sql = format!(
        "SELECT {INVENTORY_LOCATION_COLUMNS} FROM biz_inventory_location \
         WHERE material_id = ? AND warehouse_id = ? ORDER BY location_id ASC"
    );
    let rows = db()
        .find_with_params(
            &sql,
            vec![Value::from(material_id), Value::from(warehouse_id)],
        )
        .await?;
    rows.iter().map(InventoryLocationRow::from_row).collect()
}

/// 库位库存分页（按物料 / 仓库 / 库位）。
pub async fn inventory_location_page(
    q: &InventoryLocationFilter,
) -> DbResult<(Vec<InventoryLocationRow>, i64)> {
    let mut sql =
        format!("SELECT {INVENTORY_LOCATION_COLUMNS} FROM biz_inventory_location WHERE 1 = 1");
    let mut params: Vec<Value> = Vec::new();
    if let Some(material_id) = q.material_id.as_deref() {
        sql.push_str(" AND material_id = ?");
        params.push(Value::from(parse_id(material_id)?));
    }
    if let Some(warehouse_id) = q.warehouse_id.as_deref() {
        sql.push_str(" AND warehouse_id = ?");
        params.push(Value::from(parse_id(warehouse_id)?));
    }
    if let Some(location_id) = q.location_id.as_deref() {
        sql.push_str(" AND location_id = ?");
        params.push(Value::from(parse_id(location_id)?));
    }
    page_of(
        INVENTORY_LOCATION_TARGET,
        sql,
        params,
        INVENTORY_LOCATION_ORDER,
        q.page,
        q.page_size,
        InventoryLocationRow::from_row,
    )
    .await
}

/// 库存**两级账**快照：成本账（[`InventoryRow`]）+ 数量账（各库位 [`InventoryLocationRow`]）。
///
/// # 为什么把两张表一起返回
///
/// W0 设计 §2.0 的硬结论：库位维度不塞进 `biz_inventory`（否则「物料 × 仓库 × 库位」
/// 会把成本行打散，移动加权平均失去意义）。代价是**同一次查询必须同时看两张表**，
/// 否则「仓库有 5 个、库位加起来 3 个」这种账实不符看不出来。这个结构把这层不变量
/// 变成可断言的方法（[`Self::location_qty_sum`] / [`Self::is_consistent`]）。
#[derive(Debug, Clone)]
pub struct InventoryBalance {
    /// 成本账（数量合计 + 均价 + 金额）。
    pub inventory: InventoryRow,
    /// 数量账（按库位展开，可能为空——历史数据未分库位）。
    pub locations: Vec<InventoryLocationRow>,
}

impl InventoryBalance {
    /// 各库位数量之和。
    pub fn location_qty_sum(&self) -> Decimal {
        self.locations
            .iter()
            .fold(Decimal::ZERO, |acc, location| acc + location.qty)
    }

    /// 两级账是否一致：`Σ 库位数量 == 成本账数量`。
    ///
    /// 没有库位行时按「不一致」处理（空集合求和是 0，账面却非 0 就是账实不符）；
    /// 账面数量本身为 0 且无库位行时视为一致（新物料空仓）。
    pub fn is_consistent(&self) -> bool {
        if self.locations.is_empty() {
            return self.inventory.qty.is_zero();
        }
        self.location_qty_sum() == self.inventory.qty
    }
}

/// 取「物料 × 仓库」的两级账快照。
///
/// # 返回
///
/// - 成本账不存在 → `Ok(None)`（该物料在这个仓库没有任何库存记录）；
/// - 存在 → 成本账 + 库位明细（库位明细可能为空，用 [`InventoryBalance::is_consistent`] 判定）。
pub async fn inventory_balance(
    material_id: Uuid,
    warehouse_id: Uuid,
) -> DbResult<Option<InventoryBalance>> {
    let Some(inventory) = inventory_find(material_id, warehouse_id).await? else {
        return Ok(None);
    };
    let locations = inventory_locations(material_id, warehouse_id).await?;
    Ok(Some(InventoryBalance {
        inventory,
        locations,
    }))
}

// ==================== ⑬ biz_inventory_txn 库存流水 ====================

/// `biz_inventory_txn` 列清单。
pub const INVENTORY_TXN_COLUMNS: &str = "id, material_id, warehouse_id, txn_type, biz_type, \
     doc_type, doc_id, doc_no, line_no, qty, unit_cost, amount, before_qty, before_amount, \
     before_cost, after_qty, after_amount, after_cost, period_code, operator_id, remark, \
     created_at";

/// 库存流水行（**只增不改**：没有 `updated_at`，也没有编辑接口）。
#[derive(Debug, Clone)]
pub struct InventoryTxnRow {
    /// 主键。
    pub id: Uuid,
    /// 物料。
    pub material_id: Uuid,
    /// 仓库。
    pub warehouse_id: Uuid,
    /// 出入方向：`IN` / `OUT`。
    pub txn_type: String,
    /// 业务类型（如 `PURCHASE` / `SALE` / `ADJUST`）。
    pub biz_type: String,
    /// 单据类型（如 `PURCHASE_ORDER`）。
    pub doc_type: String,
    /// 单据主键。
    pub doc_id: Uuid,
    /// 单据号（冗余，便于按号检索）。
    pub doc_no: String,
    /// 单据行号。
    pub line_no: i32,
    /// 本次数量（`> 0`，方向看 `txn_type`）。
    pub qty: Decimal,
    /// 本次单位成本（`NUMERIC(18,6)`）。
    pub unit_cost: Decimal,
    /// 本次金额（`>= 0`）。
    pub amount: Decimal,
    /// 变动前数量。
    pub before_qty: Decimal,
    /// 变动前金额。
    pub before_amount: Decimal,
    /// 变动前均价。
    pub before_cost: Decimal,
    /// 变动后数量。
    pub after_qty: Decimal,
    /// 变动后金额。
    pub after_amount: Decimal,
    /// 变动后均价。
    pub after_cost: Decimal,
    /// 会计期间编码。
    pub period_code: String,
    /// 操作人。
    pub operator_id: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 记账时间。
    pub created_at: DateTime<Utc>,
}

impl InventoryTxnRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            material_id: row.get_typed("material_id")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            txn_type: row.get_typed("txn_type")?,
            biz_type: row.get_typed("biz_type")?,
            doc_type: row.get_typed("doc_type")?,
            doc_id: row.get_typed("doc_id")?,
            doc_no: row.get_typed("doc_no")?,
            line_no: row.get_typed("line_no")?,
            qty: row.get_typed("qty")?,
            unit_cost: row.get_typed("unit_cost")?,
            amount: row.get_typed("amount")?,
            before_qty: row.get_typed("before_qty")?,
            before_amount: row.get_typed("before_amount")?,
            before_cost: row.get_typed("before_cost")?,
            after_qty: row.get_typed("after_qty")?,
            after_amount: row.get_typed("after_amount")?,
            after_cost: row.get_typed("after_cost")?,
            period_code: row.get_typed("period_code")?,
            operator_id: row.get_typed("operator_id")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
        })
    }
}

/// `biz_inventory_txn` 的分页锚点。
pub const INVENTORY_TXN_TARGET: scope::ScopeTarget =
    scope::ScopeTarget::unscoped("biz_inventory_txn");

/// 流水排序：时间倒序 → 主键倒序（与 `idx_biz_inv_txn_time` 同向，稳定分页）。
pub const INVENTORY_TXN_ORDER: &str = "ORDER BY created_at DESC, id DESC";

/// 库存流水分页（物料 / 仓库 / 单据号 / 单据类型 / 出入方向 / 期间 / 记账号区间）。
pub async fn inventory_txn_page(q: &InventoryTxnFilter) -> DbResult<(Vec<InventoryTxnRow>, i64)> {
    let mut sql = format!("SELECT {INVENTORY_TXN_COLUMNS} FROM biz_inventory_txn WHERE 1 = 1");
    let mut params: Vec<Value> = Vec::new();
    if let Some(material_id) = q.material_id.as_deref() {
        sql.push_str(" AND material_id = ?");
        params.push(Value::from(parse_id(material_id)?));
    }
    if let Some(warehouse_id) = q.warehouse_id.as_deref() {
        sql.push_str(" AND warehouse_id = ?");
        params.push(Value::from(parse_id(warehouse_id)?));
    }
    if let Some(doc_no) = q.doc_no.as_deref() {
        sql.push_str(" AND doc_no = ?");
        params.push(Value::from(doc_no));
    }
    if let Some(doc_type) = q.doc_type.as_deref() {
        sql.push_str(" AND doc_type = ?");
        params.push(Value::from(doc_type));
    }
    if let Some(txn_type) = q.txn_type.as_deref() {
        sql.push_str(" AND txn_type = ?");
        params.push(Value::from(txn_type));
    }
    if let Some(period_code) = q.period_code.as_deref() {
        sql.push_str(" AND period_code = ?");
        params.push(Value::from(period_code));
    }
    if let Some(date_from) = q.date_from {
        sql.push_str(" AND created_at >= ?");
        params.push(Value::from(day_start(date_from)));
    }
    if let Some(date_to) = q.date_to {
        // 右开区间：`< 次日零点`，避免 `<= 当日 00:00` 漏掉当天的流水
        sql.push_str(" AND created_at < ?");
        params.push(Value::from(day_start(
            date_to.succ_opt().unwrap_or(date_to),
        )));
    }
    page_of(
        INVENTORY_TXN_TARGET,
        sql,
        params,
        INVENTORY_TXN_ORDER,
        q.page,
        q.page_size,
        InventoryTxnRow::from_row,
    )
    .await
}

/// `NaiveDate` → 当日零点（UTC）。`created_at` 是 `timestamptz`，按 UTC 日界过滤。
fn day_start(date: NaiveDate) -> DateTime<Utc> {
    date.and_hms_opt(0, 0, 0)
        .map(|naive| naive.and_utc())
        .unwrap_or_else(|| DateTime::<Utc>::UNIX_EPOCH)
}

// ==================== 单元测试（真库，`ERP_DB_URL`） ====================
//
// 需要真库：本卡要证的「字段映射 / 仓库子树 / 两账一致 / Decimal 精度」都只有真 PG 能证
// （`NUMERIC(18,4)` 的标度、`text_pattern_ops` 的算子、`NULLS NOT DISTINCT` 的唯一索引）。
//
// 用法（**必须用一次性库，不要指到生产**）：
//
// ```bash
// su - postgres -c 'createdb -O cloudpivot cloudpivot_w1b'
// # 用该库起一次实例把迁移跑完（V0001..V0137），或直接用已迁移好的克隆库
// export ERP_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_w1b'
// cargo test --lib business::erp
// ```
//
// 缺 `ERP_DB_URL` 时打印 `SKIP` 并直接返回（CI 无库时 `cargo test --lib` 仍然全绿）。
//
// # 夹具
//
// 固定 UUID（`b1b0…` 段）+ `W1B-` 前缀，全部 `INSERT … ON CONFLICT DO NOTHING`，
// 重复跑不积累垃圾，也不依赖清理（一次性库的语义）。
#[cfg(test)]
mod tests {
    use super::*;

    /// 夹具 UUID：`b1b0_00NN-0000-4000-8000-0000000000NN` 形态，段号见常量后缀。
    const U_ID: &str = "b1b00001-0000-4000-8000-000000000001";
    const U_BOX_ID: &str = "b1b00001-0000-4000-8000-000000000002";
    const W_ROOT_ID: &str = "b1b00002-0000-4000-8000-000000000001";
    const W_SUB_ID: &str = "b1b00002-0000-4000-8000-000000000002";
    const W_LEAF_ID: &str = "b1b00002-0000-4000-8000-000000000003";
    const W_OTHER_ID: &str = "b1b00002-0000-4000-8000-000000000004";
    const L_A1_ID: &str = "b1b00003-0000-4000-8000-000000000001";
    const L_A2_ID: &str = "b1b00003-0000-4000-8000-000000000002";
    const L_B1_ID: &str = "b1b00003-0000-4000-8000-000000000003";
    const M_RAW_ID: &str = "b1b00004-0000-4000-8000-000000000001";
    const M_SEMI_ID: &str = "b1b00004-0000-4000-8000-000000000002";
    const M_FIN_ID: &str = "b1b00004-0000-4000-8000-000000000003";
    const SUPPLIER_ID: &str = "b1b00005-0000-4000-8000-000000000001";
    const ACC_ROOT_ID: &str = "b1b00006-0000-4000-8000-000000000001";
    const ACC_CHILD_ID: &str = "b1b00006-0000-4000-8000-000000000002";
    const PERIOD_ID: &str = "b1b00007-0000-4000-8000-000000000001";
    const SEQ_ID: &str = "b1b00008-0000-4000-8000-000000000001";
    const PO_ID: &str = "b1b00009-0000-4000-8000-000000000001";
    const PO_ITEM_ID: &str = "b1b0000a-0000-4000-8000-000000000001";
    const INV_ID: &str = "b1b0000b-0000-4000-8000-000000000001";
    const INV_LOC_A1_ID: &str = "b1b0000c-0000-4000-8000-000000000001";
    const INV_LOC_A2_ID: &str = "b1b0000c-0000-4000-8000-000000000002";
    const TXN_ID: &str = "b1b0000d-0000-4000-8000-000000000001";

    /// 夹具 SQL：13 张表各若干行（金额一律用字符串，钉住 `NUMERIC(18,4)` 的标度）。
    const FIXTURES: &[&str] = &[
        // 单位：1 基础 + 1 折算（12 个/箱）
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
         VALUES ('b1b00001-0000-4000-8000-000000000001', 'W1B-PCS', '个', 1, NULL, 1.000000, 1, 1),
                ('b1b00001-0000-4000-8000-000000000002', 'W1B-BOX', '箱', 0,
                 'b1b00001-0000-4000-8000-000000000001', 12.000000, 1, 2)
         ON CONFLICT DO NOTHING",
        // 仓库：三层 + 另一棵树的顶级仓
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, allow_negative,
                                    status, sort_order, parent_id, level, warehouse_path, is_leaf)
         VALUES ('b1b00002-0000-4000-8000-000000000001', 'WH-W1B-ROOT', 'W1B 总仓', 'NORMAL', 0, 1, 1,
                 NULL, 1, '/b1b00002-0000-4000-8000-000000000001/', 0),
                ('b1b00002-0000-4000-8000-000000000002', 'WH-W1B-SUB', 'W1B 分仓', 'RAW', 0, 1, 1,
                 'b1b00002-0000-4000-8000-000000000001', 2,
                 '/b1b00002-0000-4000-8000-000000000001/b1b00002-0000-4000-8000-000000000002/', 0),
                ('b1b00002-0000-4000-8000-000000000003', 'WH-W1B-LEAF', 'W1B 库房', 'FINISHED', 0, 1, 1,
                 'b1b00002-0000-4000-8000-000000000002', 3,
                 '/b1b00002-0000-4000-8000-000000000001/b1b00002-0000-4000-8000-000000000002/b1b00002-0000-4000-8000-000000000003/', 1),
                ('b1b00002-0000-4000-8000-000000000004', 'WH-W1B-OTHER', 'W1B 另一个总仓', 'NORMAL', 0, 1, 2,
                 NULL, 1, '/b1b00002-0000-4000-8000-000000000004/', 1)
         ON CONFLICT DO NOTHING",
        // 库位：库房 2 个（A-01 默认）+ 分仓 1 个
        "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, location_type,
                                   is_default, capacity, status, sort_order)
         VALUES ('b1b00003-0000-4000-8000-000000000001', 'A-01', 'A 区一号', 'b1b00002-0000-4000-8000-000000000003',
                 'STORAGE', 1, 100.0000, 1, 1),
                ('b1b00003-0000-4000-8000-000000000002', 'A-02', 'A 区二号', 'b1b00002-0000-4000-8000-000000000003',
                 'PICKING', 0, NULL, 1, 2),
                ('b1b00003-0000-4000-8000-000000000003', 'B-01', 'B 区一号', 'b1b00002-0000-4000-8000-000000000002',
                 'STORAGE', 1, NULL, 1, 1)
         ON CONFLICT DO NOTHING",
        // 物料：3 个（RAW / SEMI / FINISHED），编码前缀 W1B-
        "INSERT INTO biz_material (id, material_code, material_name, spec, unit_id, material_type,
                                   default_warehouse_id, safety_stock, costing_method, tax_rate, status)
         VALUES ('b1b00004-0000-4000-8000-000000000001', 'W1B-M-001', 'W1B 螺栓', 'M8x20',
                 'b1b00001-0000-4000-8000-000000000001', 'RAW', 'b1b00002-0000-4000-8000-000000000003',
                 10.5000, 'MOVING_AVG', 0.1300, 1),
                ('b1b00004-0000-4000-8000-000000000002', 'W1B-M-002', 'W1B 半成品', NULL,
                 'b1b00001-0000-4000-8000-000000000001', 'SEMI', NULL, 0.0000, 'MOVING_AVG', 0.1300, 1),
                ('b1b00004-0000-4000-8000-000000000003', 'W1B-M-003', 'W1B 成品', NULL,
                 'b1b00001-0000-4000-8000-000000000002', 'FINISHED', NULL, 0.0000, 'FIFO', 0.1300, 1)
         ON CONFLICT DO NOTHING",
        // 供应商（信用额度 = 12345.6789，钉 NUMERIC(18,4)）
        "INSERT INTO biz_supplier (id, supplier_code, supplier_name, short_name, contact_person,
                                   credit_days, credit_limit, status)
         VALUES ('b1b00005-0000-4000-8000-000000000001', 'W1B-SUP-001', 'W1B 供应商', 'W1B 供应',
                 '张三', 30, 12345.6789, 1)
         ON CONFLICT DO NOTHING",
        // 科目：一级 + 二级
        "INSERT INTO biz_account (id, account_code, account_name, parent_id, level, account_category,
                                  balance_direction, is_leaf, status)
         VALUES ('b1b00006-0000-4000-8000-000000000001', 'W1B1001', 'W1B 库存现金', NULL, 1, 'ASSET', 'DEBIT', 0, 1),
                ('b1b00006-0000-4000-8000-000000000002', 'W1B100101', 'W1B 人民币现金',
                 'b1b00006-0000-4000-8000-000000000001', 2, 'ASSET', 'DEBIT', 1, 1)
         ON CONFLICT DO NOTHING",
        // 自愈：早先的夹具版本用 1001 / 100101 当科目编码（与种子撞车后被跳过），
        // 重跑时把这两行纠正成 W1B 前缀，保证断言码与库里一致（幂等）。
        "UPDATE biz_account SET account_code = 'W1B1001'
          WHERE id = 'b1b00006-0000-4000-8000-000000000001' AND account_code <> 'W1B1001'",
        "UPDATE biz_account SET account_code = 'W1B100101'
          WHERE id = 'b1b00006-0000-4000-8000-000000000002' AND account_code <> 'W1B100101'",
        // 会计期间
        "INSERT INTO biz_accounting_period (id, period_year, period_month, period_code, start_date,
                                            end_date, status)
         VALUES ('b1b00007-0000-4000-8000-000000000001', 2025, 12, '2025-12', '2025-12-01',
                 '2025-12-31', 'OPEN')
         ON CONFLICT DO NOTHING",
        // 单号序列（当前序号 7，步长 1）
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no,
                              step, separator, status)
         VALUES ('b1b00008-0000-4000-8000-000000000001', 'W1B-PO', 'W1B 采购订单号', 'W1B', 'yyyyMMdd',
                 '20261030', 7, 1, '-', 1)
         ON CONFLICT DO NOTHING",
        // 采购订单（含税 = 不含税 + 税，DB 约束会校验）
        "INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, expect_date,
                                         warehouse_id, status, currency_code, total_qty, total_amount,
                                         total_tax, total_with_tax, credit_days)
         VALUES ('b1b00009-0000-4000-8000-000000000001', 'W1B-PO-0001',
                 'b1b00005-0000-4000-8000-000000000001', '2026-10-29', '2026-11-05',
                 'b1b00002-0000-4000-8000-000000000003', 'DRAFT', 'CNY', 5.0000, 100.0000, 13.0000,
                 113.0000, 30)
         ON CONFLICT DO NOTHING",
        // 采购明细（金额 100 + 税 13 = 113）
        "INSERT INTO biz_purchase_order_item (id, order_id, line_no, material_id, unit_id, qty, price,
                                              tax_rate, amount, tax_amount, with_tax_amount, received_qty)
         VALUES ('b1b0000a-0000-4000-8000-000000000001', 'b1b00009-0000-4000-8000-000000000001', 1,
                 'b1b00004-0000-4000-8000-000000000001', 'b1b00001-0000-4000-8000-000000000001',
                 5.0000, 20.000000, 0.1300, 100.0000, 13.0000, 113.0000, 0.0000)
         ON CONFLICT DO NOTHING",
        // 成本账：5 个、均价 20、金额 100
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, qty, locked_qty, avg_cost, amount)
         VALUES ('b1b0000b-0000-4000-8000-000000000001', 'b1b00004-0000-4000-8000-000000000001',
                 'b1b00002-0000-4000-8000-000000000003', 5.0000, 0.0000, 20.000000, 100.0000)
         ON CONFLICT DO NOTHING",
        // 数量账：A-01 放 3、A-02 放 2（合计 = 成本账 5）
        "INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty, locked_qty)
         VALUES ('b1b0000c-0000-4000-8000-000000000001', 'b1b00004-0000-4000-8000-000000000001',
                 'b1b00002-0000-4000-8000-000000000003', 'b1b00003-0000-4000-8000-000000000001',
                 3.0000, 0.0000),
                ('b1b0000c-0000-4000-8000-000000000002', 'b1b00004-0000-4000-8000-000000000001',
                 'b1b00002-0000-4000-8000-000000000003', 'b1b00003-0000-4000-8000-000000000002',
                 2.0000, 0.0000)
         ON CONFLICT DO NOTHING",
        // 流水：入库 5 个
        "INSERT INTO biz_inventory_txn (id, material_id, warehouse_id, txn_type, biz_type, doc_type,
                                        doc_id, doc_no, line_no, qty, unit_cost, amount, before_qty,
                                        before_amount, before_cost, after_qty, after_amount, after_cost,
                                        period_code)
         VALUES ('b1b0000d-0000-4000-8000-000000000001', 'b1b00004-0000-4000-8000-000000000001',
                 'b1b00002-0000-4000-8000-000000000003', 'IN', 'PURCHASE', 'PURCHASE_ORDER',
                 'b1b00009-0000-4000-8000-000000000001', 'W1B-PO-0001', 1, 5.0000, 20.000000, 100.0000,
                 0.0000, 0.0000, 0.000000, 5.0000, 100.0000, 20.000000, '2026-10')
         ON CONFLICT DO NOTHING",
    ];

    /// 真库用例**串行**执行：36 个用例共用一个库，而
    /// `material_page_serial_required_filter_tracks_column_existence` 会 `ALTER TABLE`
    /// （ACCESS EXCLUSIVE）——并行跑时其它用例的查询会被挂住甚至被断开
    /// （实测 `query failed … : connection closed`）。锁的粒度 = 一个用例，时间在毫秒级。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 初始化全局数据库（进程内只做一次）；`false` = 没配 `ERP_DB_URL`，用例直接跳过。
    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                // 每个 `#[tokio::test]` 都是**独立的 tokio runtime**，而 `db()` 是进程级全局池：
                // 池里空闲的连接是绑在「上一个 runtime」上的（`tokio::spawn(connection)`），
                // runtime 一结束那条连接就死了，但 `client.is_closed()` 不会立刻变 true
                // ⇒ 下一次借出会拿到死连接，报 `query failed … : connection closed`（实测 36 条里挂 5–7 条）。
                // 对策：把 `max_lifetime` 设成 0，让池每次都丢掉空闲连接、按需新建
                // （`PostgresPool::get` 的 `pop_available` 只认 is_available → 只剩新建一条路）。
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => match set_db(connection) {
                        Ok(()) => true,
                        Err(err) => {
                            println!("SKIP: set_db 失败（{err}）");
                            false
                        }
                    },
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 拿锁 + 连库 + 建夹具（幂等）。返回 `None` = 没配 `ERP_DB_URL`，用例应直接返回。
    ///
    /// 返回的 guard 必须活到用例结束（`let _db = ...`），否则锁会被提前释放。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        for sql in FIXTURES {
            if let Err(err) = db().execute_with_params(sql, vec![]).await {
                panic!("夹具写入失败：{err}\nSQL: {sql}");
            }
        }
        Some(guard)
    }

    fn uuid(text: &str) -> Uuid {
        Uuid::parse_str(text).expect("夹具 UUID 非法")
    }

    fn dec(text: &str) -> Decimal {
        use std::str::FromStr;
        Decimal::from_str(text).expect("测试金额字面量非法")
    }

    // ---------- ① 13 张表的 Row 字段映射 ----------

    #[tokio::test]
    async fn unit_row_maps_key_field() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let row = unit_find_by_id(uuid(U_ID))
            .await
            .unwrap()
            .expect("单位应存在");
        assert_eq!(row.unit_code, "W1B-PCS");
        assert_eq!(row.is_base, 1);
        assert!(row.base_unit_id.is_none());
        let boxed = unit_find_by_id(uuid(U_BOX_ID)).await.unwrap().unwrap();
        assert_eq!(boxed.base_unit_id, Some(uuid(U_ID)));
        assert_eq!(boxed.rate, dec("12"));
    }

    #[tokio::test]
    async fn warehouse_row_maps_level_and_path() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let leaf = warehouse_find_by_id(uuid(W_LEAF_ID))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(leaf.level, 3);
        assert_eq!(leaf.is_leaf, 1);
        assert_eq!(leaf.warehouse_type, "FINISHED");
        assert_eq!(
            leaf.warehouse_path,
            format!("/{W_ROOT_ID}/{W_SUB_ID}/{W_LEAF_ID}/")
        );
        assert_eq!(leaf.parent_id, Some(uuid(W_SUB_ID)));
    }

    #[tokio::test]
    async fn location_row_maps_warehouse_and_default() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let row = location_find_by_id(uuid(L_A1_ID)).await.unwrap().unwrap();
        assert_eq!(row.location_code, "A-01");
        assert_eq!(row.warehouse_id, uuid(W_LEAF_ID));
        assert_eq!(row.is_default, 1);
        assert_eq!(row.location_type, "STORAGE");
        assert_eq!(row.capacity, Some(dec("100")));

        // 按仓库查库位：库房 2 个（sort_order 升序），分仓 1 个 → L_A2 / L_B1 都用到
        let leaf_locations = location_by_warehouse(uuid(W_LEAF_ID), false).await.unwrap();
        assert_eq!(leaf_locations.len(), 2);
        assert_eq!(leaf_locations[1].id, uuid(L_A2_ID));
        let sub_locations = location_by_warehouse(uuid(W_SUB_ID), false).await.unwrap();
        assert_eq!(sub_locations.len(), 1);
        assert_eq!(sub_locations[0].id, uuid(L_B1_ID));
    }

    #[tokio::test]
    async fn material_row_maps_type_and_tax() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let row = material_find_by_code("W1B-M-001")
            .await
            .unwrap()
            .expect("物料应存在");
        assert_eq!(row.material_type, "RAW");
        assert_eq!(row.costing_method, "MOVING_AVG");
        assert_eq!(row.tax_rate, dec("0.13"));
        assert_eq!(row.safety_stock, dec("10.5"));
        assert_eq!(row.default_warehouse_id, Some(uuid(W_LEAF_ID)));
    }

    #[tokio::test]
    async fn supplier_row_maps_credit_limit() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let row = supplier_find_by_id(uuid(SUPPLIER_ID))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.supplier_code, "W1B-SUP-001");
        assert_eq!(row.credit_days, 30);
        assert_eq!(row.credit_limit, dec("12345.6789"));
    }

    #[tokio::test]
    async fn account_row_maps_direction_and_leaf() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let child = account_find_by_code("W1B100101").await.unwrap().unwrap();
        assert_eq!(child.account_category, "ASSET");
        assert_eq!(child.balance_direction, "DEBIT");
        assert_eq!(child.is_leaf, 1);
        assert_eq!(child.level, 2);
        assert_eq!(child.parent_id, Some(uuid(ACC_ROOT_ID)));
        let children = account_children(uuid(ACC_ROOT_ID)).await.unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, uuid(ACC_CHILD_ID));
    }

    #[tokio::test]
    async fn period_row_maps_code_and_status() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let row = period_find_by_code("2025-12").await.unwrap().unwrap();
        assert_eq!(row.id, uuid(PERIOD_ID));
        assert_eq!(row.period_year, 2025);
        assert_eq!(row.period_month, 12);
        assert_eq!(row.status, "OPEN");
        assert_eq!(row.start_date.to_string(), "2025-12-01");
    }

    #[tokio::test]
    async fn seq_row_maps_current_no_and_step() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let row = seq_find_by_type("W1B-PO").await.unwrap().unwrap();
        assert_eq!(row.current_no, 7);
        assert_eq!(row.step, 1);
        assert_eq!(row.date_pattern, "yyyyMMdd");
        assert_eq!(row.prefix, "W1B");
    }

    #[tokio::test]
    async fn purchase_order_row_maps_amounts_and_currency() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let row = purchase_order_find_by_no("W1B-PO-0001")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "DRAFT");
        assert_eq!(row.currency_code, "CNY", "CHAR(3) 已 ::text，不带补位空格");
        assert_eq!(row.total_qty, dec("5"));
        assert_eq!(row.total_amount, dec("100"));
        assert_eq!(row.total_tax, dec("13"));
        assert_eq!(row.total_with_tax, dec("113"));
    }

    #[tokio::test]
    async fn purchase_order_item_row_maps_qty_and_received() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let items = purchase_order_items(uuid(PO_ID)).await.unwrap();
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.id, uuid(PO_ITEM_ID));
        assert_eq!(item.line_no, 1);
        assert_eq!(item.qty, dec("5"));
        assert_eq!(item.price, dec("20"), "单价 NUMERIC(18,6) 读回 20");
        assert_eq!(item.received_qty, Decimal::ZERO);
        assert_eq!(item.with_tax_amount, item.amount + item.tax_amount);
    }

    #[tokio::test]
    async fn inventory_row_maps_avg_cost() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let row = inventory_find(uuid(M_RAW_ID), uuid(W_LEAF_ID))
            .await
            .unwrap()
            .expect("库存应存在");
        assert_eq!(row.id, uuid(INV_ID));
        assert_eq!(row.qty, dec("5"));
        assert_eq!(row.avg_cost, dec("20"), "均价 NUMERIC(18,6)");
        assert_eq!(row.amount, dec("100"));
        let missing = inventory_find(uuid(M_SEMI_ID), uuid(W_LEAF_ID))
            .await
            .unwrap();
        assert!(missing.is_none(), "没库存的物料返回 None，而不是空行");
    }

    #[tokio::test]
    async fn inventory_location_row_maps_qty_per_location() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let rows = inventory_locations(uuid(M_RAW_ID), uuid(W_LEAF_ID))
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, uuid(INV_LOC_A1_ID));
        assert_eq!(rows[0].location_id, Some(uuid(L_A1_ID)));
        assert_eq!(rows[0].qty, dec("3"));
        assert_eq!(rows[1].id, uuid(INV_LOC_A2_ID));
        assert_eq!(rows[1].qty, dec("2"));
    }

    #[tokio::test]
    async fn inventory_txn_row_maps_direction_and_qty() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let filter = InventoryTxnFilter {
            doc_no: Some("W1B-PO-0001".to_string()),
            ..Default::default()
        };
        let (rows, total) = inventory_txn_page(&filter).await.unwrap();
        assert_eq!(total, 1);
        let txn = &rows[0];
        assert_eq!(txn.id, uuid(TXN_ID));
        assert_eq!(txn.txn_type, "IN");
        assert_eq!(txn.qty, dec("5"));
        assert_eq!(txn.before_qty, Decimal::ZERO);
        assert_eq!(txn.after_qty, dec("5"));
        assert_eq!(txn.period_code, "2026-10");
    }

    // ---------- ② 仓库树（3 层，物化路径前缀） ----------

    #[tokio::test]
    async fn warehouse_tree_returns_three_levels_in_path_order() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let tree = warehouse_tree(Some(uuid(W_ROOT_ID))).await.unwrap();
        let codes: Vec<&str> = tree.iter().map(|w| w.warehouse_code.as_str()).collect();
        assert_eq!(
            codes,
            vec!["WH-W1B-ROOT", "WH-W1B-SUB", "WH-W1B-LEAF"],
            "子树 = 自身 + 后代，按物化路径先序"
        );
        assert_eq!(tree[0].level, 1);
        assert_eq!(tree[1].level, 2);
        assert_eq!(tree[2].level, 3);
    }

    #[tokio::test]
    async fn warehouse_subtree_excludes_sibling_root() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let tree = warehouse_tree(Some(uuid(W_SUB_ID))).await.unwrap();
        assert_eq!(tree.len(), 2, "分仓子树 = 分仓 + 库房");
        assert!(
            tree.iter().all(|w| w.id != uuid(W_OTHER_ID)),
            "另一个总仓不能出现在分仓的子树里"
        );
        let leaf_only = warehouse_tree(Some(uuid(W_LEAF_ID))).await.unwrap();
        assert_eq!(leaf_only.len(), 1, "叶子仓库子树只有自己");
        assert_eq!(leaf_only[0].id, uuid(W_LEAF_ID));
        let missing = warehouse_tree(Some(Uuid::now_v7())).await.unwrap();
        assert!(missing.is_empty(), "根不存在 → 空子树（不报错）");
    }

    #[tokio::test]
    async fn warehouse_child_counts_and_children() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let counts = warehouse_child_counts(&[uuid(W_ROOT_ID), uuid(W_SUB_ID), uuid(W_OTHER_ID)])
            .await
            .unwrap();
        assert_eq!(counts.get(&uuid(W_ROOT_ID)), Some(&1));
        assert_eq!(counts.get(&uuid(W_SUB_ID)), Some(&1));
        assert_eq!(
            counts.get(&uuid(W_OTHER_ID)),
            None,
            "无子节点不出现在 map 里"
        );
        let children = warehouse_children(uuid(W_ROOT_ID)).await.unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, uuid(W_SUB_ID));
    }

    #[test]
    fn path_prefix_upper_bounds_the_subtree() {
        assert_eq!(path_prefix_upper("/a/b/"), "/a/b0");
        assert_eq!(path_prefix_upper("/a/b"), "/a/b0");
        // 前缀区间语义自证：所有后代落在 [lo, hi) 内，另一个顶级仓在外面
        let lo = "/root/";
        let hi = path_prefix_upper(lo);
        let descendant = format!("{lo}sub/");
        assert!(descendant.as_str() >= lo && descendant.as_str() < hi.as_str());
        let sibling = "/other/";
        assert!(!(sibling >= lo && sibling < hi.as_str()));
    }

    // ---------- ③ 物料分页 + 一物一码筛选 ----------

    #[tokio::test]
    async fn material_page_filters_and_pages() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let filter = MaterialFilter {
            keyword: Some("W1B-M-".to_string()),
            page: 2,
            page_size: 2,
            ..Default::default()
        };
        let (rows, total) = material_page(&filter).await.unwrap();
        assert_eq!(total, 3, "W1B- 前缀共 3 个物料");
        assert_eq!(rows.len(), 1, "第 2 页只剩 1 条");
        assert_eq!(rows[0].material_code, "W1B-M-003");

        let only_semi = MaterialFilter {
            material_type: Some("SEMI".to_string()),
            keyword: Some("W1B-".to_string()),
            ..Default::default()
        };
        let (rows, total) = material_page(&only_semi).await.unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows[0].id, uuid(M_SEMI_ID));
    }

    #[tokio::test]
    async fn material_page_serial_required_filter_tracks_column_existence() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let had_column = material_has_serial_column().await.unwrap();
        if !had_column {
            // V0137 现状：列属 V0138 → 显式报错（不静默返回全量）
            let err = material_page(&MaterialFilter {
                serial_required: Some(true),
                ..Default::default()
            })
            .await
            .expect_err("列不存在时必须报错");
            assert!(
                matches!(&err, DbError::InvalidValue(message) if message.contains("serial_required")),
                "错误必须点名 serial_required，实际：{err:?}"
            );
            db().execute_with_params(
                "ALTER TABLE biz_material ADD COLUMN serial_required SMALLINT NOT NULL DEFAULT 0",
                vec![],
            )
            .await
            .expect("造列（模拟 V0138）");
            db().execute_with_params(
                "UPDATE biz_material SET serial_required = 1 \
                 WHERE id = 'b1b00004-0000-4000-8000-000000000003'",
                vec![],
            )
            .await
            .expect("标记一物一码物料");
        }

        let (rows, total) = material_page(&MaterialFilter {
            serial_required: Some(true),
            keyword: Some("W1B-".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(total, 1, "只有 W1B-M-003 是一物一码");
        assert_eq!(rows[0].material_code, "W1B-M-003");

        let (_, not_serial) = material_page(&MaterialFilter {
            serial_required: Some(false),
            keyword: Some("W1B-".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(not_serial, 2);

        if !had_column {
            // 恢复原样：不把「模拟 V0138」的列留在库里
            db().execute_with_params(
                "ALTER TABLE biz_material DROP COLUMN serial_required",
                vec![],
            )
            .await
            .expect("回滚造列");
        }
    }

    // ---------- ④ 库存两级账 ----------

    #[tokio::test]
    async fn inventory_two_level_ledger_is_consistent() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let balance = inventory_balance(uuid(M_RAW_ID), uuid(W_LEAF_ID))
            .await
            .unwrap()
            .expect("两级账应存在");
        assert_eq!(balance.inventory.qty, dec("5"), "成本账数量");
        assert_eq!(balance.locations.len(), 2, "数量账按库位拆成 2 行");
        assert_eq!(
            balance.location_qty_sum(),
            dec("5"),
            "库位数量之和 == 成本账数量"
        );
        assert!(balance.is_consistent());

        let missing = inventory_balance(uuid(M_FIN_ID), uuid(W_LEAF_ID))
            .await
            .unwrap();
        assert!(missing.is_none(), "无库存记录 → None");
    }

    #[test]
    fn inventory_balance_consistency_rules() {
        // 纯逻辑：没有库位行时，账面 0 视为一致、账面非 0 视为不一致
        let empty = InventoryBalance {
            inventory: fake_inventory_row(Decimal::ZERO),
            locations: Vec::new(),
        };
        assert!(empty.is_consistent());
        let broken = InventoryBalance {
            inventory: fake_inventory_row(dec("5")),
            locations: Vec::new(),
        };
        assert!(!broken.is_consistent(), "账面有数量却没有库位行 = 账实不符");
    }

    fn fake_inventory_row(qty: Decimal) -> InventoryRow {
        let now = DateTime::<Utc>::UNIX_EPOCH;
        InventoryRow {
            id: Uuid::now_v7(),
            material_id: Uuid::now_v7(),
            warehouse_id: Uuid::now_v7(),
            qty,
            locked_qty: Decimal::ZERO,
            avg_cost: Decimal::ZERO,
            amount: Decimal::ZERO,
            last_txn_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    // ---------- ⑥ Decimal 精度（NUMERIC(18,4)） ----------

    #[tokio::test]
    async fn decimal_12345_6789_roundtrips_without_loss() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        // 供应商信用额度列是 NUMERIC(18,4)：写 12345.6789 → 读回必须逐位一致
        let row = supplier_find_by_id(uuid(SUPPLIER_ID))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.credit_limit, dec("12345.6789"));
        assert_eq!(
            row.credit_limit.to_string(),
            "12345.6789",
            "标度不丢（不是 12345.6790）"
        );

        // 写入侧同样定点：更新成 0.0001（4 位小数的最小单位）再读回
        db().execute_with_params(
            "UPDATE biz_supplier SET credit_limit = ? WHERE id = ?",
            vec![
                TypeMapper::to_value(&dec("0.0001")),
                Value::from(uuid(SUPPLIER_ID)),
            ],
        )
        .await
        .expect("写入 0.0001");
        let after = supplier_find_by_id(uuid(SUPPLIER_ID))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.credit_limit, dec("0.0001"));
        // 复原，避免影响同库其它用例的断言
        db().execute_with_params(
            "UPDATE biz_supplier SET credit_limit = ? WHERE id = ?",
            vec![
                TypeMapper::to_value(&dec("12345.6789")),
                Value::from(uuid(SUPPLIER_ID)),
            ],
        )
        .await
        .expect("复原信用额度");
    }

    // ---------- ⑦ 分页口径 ----------

    #[test]
    fn normalize_page_clamps_bounds() {
        assert_eq!(normalize_page(0, 0), (1, DEFAULT_PAGE_SIZE));
        assert_eq!(normalize_page(3, 50), (3, 50));
        assert_eq!(normalize_page(1, 10_000), (1, MAX_PAGE_SIZE));
    }

    #[tokio::test]
    async fn list_pages_respect_filters() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (units, unit_total) = unit_page(&UnitFilter {
            keyword: Some("W1B-".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(unit_total, 2);
        assert!(units.iter().all(|u| u.unit_code.starts_with("W1B-")));

        let (locs, loc_total) = location_page(&LocationFilter {
            warehouse_id: Some(uuid(W_LEAF_ID).to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(loc_total, 2, "库房只有 2 个库位");
        assert_eq!(locs[0].location_code, "A-01", "按 sort_order 升序");

        let (roots, root_total) = warehouse_page(&WarehouseFilter {
            keyword: Some("W1B".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(root_total, 4, "W1B 前缀共 4 个仓库");
        assert_eq!(roots.len(), 4, "一页装得下 4 个");

        let (pos, po_total) = purchase_order_page(&PurchaseOrderFilter {
            status: Some("DRAFT".to_string()),
            date_from: Some(NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()),
            date_to: Some(NaiveDate::from_ymd_opt(2026, 10, 31).unwrap()),
            keyword: Some("W1B-PO".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(po_total, 1);
        assert_eq!(pos[0].id, uuid(PO_ID));

        let (periods, period_total) = period_page(&PeriodFilter {
            period_year: Some(2025),
            ..Default::default()
        })
        .await
        .unwrap();
        assert!(period_total >= 1);
        assert!(periods.iter().any(|p| p.id == uuid(PERIOD_ID)));

        let (seqs, seq_total) = seq_page(&SeqFilter {
            keyword: Some("W1B".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(seq_total, 1);
        assert_eq!(seqs[0].id, uuid(SEQ_ID));

        let (accounts, acc_total) = account_page(&AccountFilter {
            keyword: Some("W1B".to_string()),
            is_leaf: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(acc_total, 1);
        assert_eq!(accounts[0].id, uuid(ACC_CHILD_ID));

        let (suppliers, sup_total) = supplier_page(&SupplierFilter {
            keyword: Some("W1B-SUP".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(sup_total, 1);
        assert_eq!(suppliers[0].id, uuid(SUPPLIER_ID));

        let (inv_rows, inv_total) = inventory_page(&InventoryFilter {
            warehouse_id: Some(uuid(W_LEAF_ID).to_string()),
            only_nonzero: true,
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(inv_total, 1);
        assert_eq!(inv_rows[0].id, uuid(INV_ID));

        let (inv_locs, inv_loc_total) = inventory_location_page(&InventoryLocationFilter {
            warehouse_id: Some(uuid(W_LEAF_ID).to_string()),
            material_id: Some(uuid(M_RAW_ID).to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(inv_loc_total, 2);
        assert!(inv_locs.iter().all(|l| l.qty > Decimal::ZERO));

        let (items, item_total) = purchase_order_item_page(&PurchaseOrderItemFilter {
            order_id: Some(uuid(PO_ID).to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(item_total, 1);
        assert_eq!(items[0].id, uuid(PO_ITEM_ID));
    }
}

// ============================================================================
// model_master.rs —— 主数据只读：计量单位 / 仓库 / 库位 / 物料 / 供应商 / 会计科目 / 会计期间 / 取号规则
//
// 由 `model.rs` 按职责拆出（TD-1 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use std::collections::HashMap;

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::dto::{
    AccountFilter, LocationFilter, MaterialFilter, PeriodFilter, SeqFilter, SupplierFilter,
    UnitFilter, WarehouseFilter,
};
use crate::business::shared::scope;
use crate::utils;

use super::model_base::*;

/// `biz_unit` 列清单（[`UnitRow::from_row`] 与所有 SELECT 共用）。
pub const UNIT_COLUMNS: &str = "id, unit_code, unit_name, is_base, base_unit_id, rate, status, \
     sort_order, remark, is_system, precision, created_at, updated_at, create_by, update_by, \
     deleted_at";

/// 计量单位行。
///
/// `is_base = 1` 表示基础单位（此时 `base_unit_id` 必为 NULL）；
/// `is_base = 0` 时 `rate` 是「1 个本单位 = `rate` 个基础单位」（`NUMERIC(18,6)`）。
/// `is_system = true` 是 ERP-S1（V0187）预置的标准值：只能停用，不能删除。
/// `precision` 是数量小数位数（0–6，V0187 新增列）。
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
    /// 是否系统预置（ERP-S1）：预置单位不能删除，只能停用。
    pub is_system: bool,
    /// 数量小数位数（0–6）。
    pub precision: i16,
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
            is_system: row.get_typed("is_system")?,
            precision: row.get_typed("precision")?,
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
     tax_rate, status, serial_required, remark, created_at, updated_at, create_by, update_by, \
     deleted_at";

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
    /// 是否强制一物一码（V0138 加的列；`TRUE` 时出入库必须逐件登记序列号）。
    pub serial_required: bool,
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
            serial_required: row.get_typed("serial_required")?,
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
        // 列是 **BOOLEAN**（V0138 定稿）：这里必须绑 bool，绑 0/1 会让 PG 报
        // `operator does not exist: boolean = smallint`（与 V0137 §5.9 那个地雷同源）。
        sql.push_str(" AND serial_required = ?");
        params.push(Value::from(serial_required));
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

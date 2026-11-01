//! ERP 接口契约（DTO）：13 张 `biz_*` 表各 4–5 个结构（建 / 改 / 筛选 / 卡片 / 详情）。
//!
//! 依据：`V0137` 的列定义（长度、精度、枚举白名单逐列对齐），
//! 与 `business/task/dto.rs` 的约定保持一致，让前端不必为 ERP 单独适配。
//!
//! # 命名与格式约定（与批次 A/B/C 一致）
//!
//! - 主键 / 外键一律 `String`（`nik::uuid::Uuid` 没开 serde feature，序列化不了）；
//! - 日期 `chrono::NaiveDate`（JSON `"2026-10-30"`，对应 `DATE` 列）；
//! - 时间戳 `chrono::DateTime<Utc>`（JSON RFC3339，对应 `TIMESTAMPTZ` 列）；
//! - 枚举一律大写字符串，**与库里的 CHECK 约束逐字一致**（`RAW` / `DRAFT` / `OPEN` …）；
//! - 金额 / 数量 / 单价一律 [`Decimal`]（`rust_decimal`，W0 设计 §2.0.3）：
//!   数量金额 `NUMERIC(18,4)`、单价均价 `NUMERIC(18,6)`、税率 `NUMERIC(7,4)`。
//!   用 `f64` 会在「0.1 + 0.2」这种地方与库里的定点值分叉；用 `String` 又得在每层手动解析。
//!
//! # 编辑接口的「字段缺席 ≠ 清空」
//!
//! `UpdateXxxReq` 的可空字段一律 `Option<Option<T>>` + `#[serde(default, deserialize_with =
//! "double_option")]`：字段缺席（`None`）→ 保持原值；显式 `null`（`Some(None)`）→ 清空；
//! 有值（`Some(Some(v))`）→ 赋值。不可空的字段用平铺 `Option<T>`（缺席 = 不改，传 null 会被
//! 反序列化成 `None` 从而等于不改）。
//!
//! # 校验在哪一层
//!
//! 卡面：「`CreateXxxReq` 建（含 validate 校验）」。所以每个 Create 都有一个
//! `pub fn validate(&self) -> std::result::Result<(), ApiError>`：
//! **接口层 400 的中文文案**由它给出，数据库的 `CHECK` / 唯一索引只是兜底。
//! 校验口径与库逐列对齐（长度取 `VARCHAR(n)` 的 n、枚举取 `CHECK (... IN (...))` 的集合），
//! 单测在文件末尾钉住「空 code / 超长 name / 负数量」三类必拒。
//!
//! # 与卡面的差异
//!
//! - `biz_inventory_txn` **不提供 Update**（流水只增不改，改了就不是账）；
//!   只给 `CreateInventoryTxnReq` / `InventoryTxnFilter` / `InventoryTxnCard` / `InventoryTxnRes`。
//! - `biz_purchase_order_item` 的「建」有两种写法（随主单一起建、单独补行），
//!   这里给的是**单行**形态（`order_id` 显式给出），随单建由 W1c 在事务里逐行调用。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use serde::{Deserialize, Deserializer, Serialize};

use crate::business::erp::model::{
    AccountRow, DEFAULT_PAGE_SIZE, InventoryLocationRow, InventoryRow, InventoryTxnRow,
    LocationRow, MaterialRow, PeriodRow, PurchaseOrderItemRow, PurchaseOrderRow, SeqRow,
    SupplierRow, UnitRow, WarehouseRow,
};
use crate::shared::error_code;

// ==================== 枚举白名单（与 DB 的 CHECK 逐字一致） ====================

/// 仓库类型（`ck_biz_warehouse_type`）。
pub const WAREHOUSE_TYPES: [&str; 5] = ["NORMAL", "RAW", "FINISHED", "SCRAP", "IN_TRANSIT"];
/// 库位类型（`ck_biz_location_type`）。
pub const LOCATION_TYPES: [&str; 6] = [
    "STORAGE",
    "PICKING",
    "RECEIVING",
    "SHIPPING",
    "QUARANTINE",
    "SCRAP",
];
/// 物料类型（`ck_biz_material_type`）。
pub const MATERIAL_TYPES: [&str; 4] = ["RAW", "SEMI", "FINISHED", "SERVICE"];
/// 计价方式（`ck_biz_material_costing`）。
pub const COSTING_METHODS: [&str; 3] = ["MOVING_AVG", "FIFO", "STANDARD"];
/// 科目类别（`ck_biz_account_category`）。
pub const ACCOUNT_CATEGORIES: [&str; 5] = ["ASSET", "LIABILITY", "EQUITY", "COST", "PROFIT_LOSS"];
/// 余额方向（`ck_biz_account_direction`）。
pub const BALANCE_DIRECTIONS: [&str; 2] = ["DEBIT", "CREDIT"];
/// 会计期间状态（`ck_biz_period_status`）。
pub const PERIOD_STATUSES: [&str; 3] = ["NOT_OPENED", "OPEN", "CLOSED"];
/// 单号序列的日期段格式（`ck_biz_seq_date`）。
pub const DATE_PATTERNS: [&str; 3] = ["yyyyMMdd", "yyyyMM", "none"];
/// 采购订单状态（`ck_biz_po_status`）。
pub const PURCHASE_ORDER_STATUSES: [&str; 7] = [
    "DRAFT",
    "SUBMITTED",
    "APPROVED",
    "PARTIAL_RECEIVED",
    "RECEIVED",
    "CLOSED",
    "CANCELLED",
];
/// 库存出入方向（`ck_biz_inv_txn_type`）。
pub const TXN_TYPES: [&str; 2] = ["IN", "OUT"];

// ==================== 校验原语 ====================

/// 必填文本：去空白后非空，且 `chars().count() <= max`（与 `VARCHAR(n)` 同口径）。
fn require_text(value: &str, field: &str, max: usize) -> std::result::Result<(), ApiError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    if trimmed.chars().count() > max {
        return Err(bad_request(format!(
            "{field} 最长 {max} 个字符（当前 {} 个）",
            trimmed.chars().count()
        )));
    }
    Ok(())
}

/// 选填文本：`None` / 空串放过；有值则限长（超长同样拒，避免被 `VARCHAR(n)` 静默截断）。
fn check_optional_text(
    value: Option<&str>,
    field: &str,
    max: usize,
) -> std::result::Result<(), ApiError> {
    match value.map(str::trim) {
        Some(text) if !text.is_empty() && text.chars().count() > max => Err(bad_request(format!(
            "{field} 最长 {max} 个字符（当前 {} 个）",
            text.chars().count()
        ))),
        _ => Ok(()),
    }
}

/// 枚举白名单。
fn require_one_of(value: &str, allowed: &[&str], field: &str) -> std::result::Result<(), ApiError> {
    if allowed.contains(&value) {
        return Ok(());
    }
    Err(bad_request(format!(
        "{field} 必须是 {} 之一（当前 `{value}`）",
        allowed.join(" / ")
    )))
}

/// 启用位（`SMALLINT 1/0`）。
fn require_flag(value: i16, field: &str) -> std::result::Result<(), ApiError> {
    if value == 0 || value == 1 {
        return Ok(());
    }
    Err(bad_request(format!("{field} 只能是 1（启用）或 0（停用）")))
}

/// 非负数（`>= 0`）。
fn require_non_negative(value: Decimal, field: &str) -> std::result::Result<(), ApiError> {
    if value < Decimal::ZERO {
        return Err(bad_request(format!("{field} 不能为负数（当前 {value}）")));
    }
    Ok(())
}

/// 正数（`> 0`）。
fn require_positive(value: Decimal, field: &str) -> std::result::Result<(), ApiError> {
    if value <= Decimal::ZERO {
        return Err(bad_request(format!("{field} 必须大于 0（当前 {value}）")));
    }
    Ok(())
}

/// 税率（`0 <= rate <= 1`）。
fn require_tax_rate(value: Decimal) -> std::result::Result<(), ApiError> {
    if value < Decimal::ZERO || value > Decimal::ONE {
        return Err(bad_request(format!(
            "tax_rate 必须在 0 与 1 之间（当前 {value}，0.13 表示 13%）"
        )));
    }
    Ok(())
}

/// `400 + 10001`（参数非法）：ERP 全部校验失败都走这个码，前端按 `field` 文案提示即可。
fn bad_request(message: String) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message)
}

/// `Option<Option<T>>` 的反序列化：缺席 → `None`，`null` → `Some(None)`，有值 → `Some(Some(v))`。
fn double_option<'de, T, D>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

// ==================== ① 计量单位 ====================

/// 新建计量单位。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateUnitReq {
    /// 单位编码（`VARCHAR(20)`，未删除内唯一）。
    pub unit_code: String,
    /// 单位名称（`VARCHAR(50)`）。
    pub unit_name: String,
    /// 是否基础单位（缺省 `true`）。
    #[serde(default = "default_true")]
    pub is_base: bool,
    /// 折算基准单位（`is_base = false` 时必填）。
    #[serde(default)]
    pub base_unit_id: Option<String>,
    /// 折算率（`NUMERIC(18,6)`；`is_base = false` 时必填且 `> 0`）。
    #[serde(default)]
    pub rate: Option<Decimal>,
    /// 启用状态（缺省 1）。
    #[serde(default = "default_enabled")]
    pub status: i16,
    /// 排序值（缺省 0）。
    #[serde(default)]
    pub sort_order: i32,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

impl CreateUnitReq {
    /// 校验（`400 + 10001`）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        require_text(&self.unit_code, "unit_code", 20)?;
        require_text(&self.unit_name, "unit_name", 50)?;
        require_flag(self.status, "status")?;
        check_optional_text(self.remark.as_deref(), "remark", 2000)?;
        if self.is_base {
            if self.base_unit_id.is_some() {
                return Err(bad_request(
                    "is_base = true 时不能填 base_unit_id（基础单位没有折算基准）".to_string(),
                ));
            }
            if let Some(rate) = self.rate
                && rate != Decimal::ONE
            {
                return Err(bad_request(
                    "is_base = true 时 rate 必须为 1（基础单位的折算率是恒等的）".to_string(),
                ));
            }
            return Ok(());
        }
        if self.base_unit_id.is_none() {
            return Err(bad_request(
                "is_base = false 时必须填 base_unit_id（折算单位要指明基准）".to_string(),
            ));
        }
        match self.rate {
            Some(rate) => require_positive(rate, "rate"),
            None => Err(bad_request(
                "is_base = false 时必须填 rate（1 个本单位等于几个基础单位）".to_string(),
            )),
        }
    }
}

/// 编辑计量单位（字段缺席 = 不改）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateUnitReq {
    /// 单位名称。
    #[serde(default)]
    pub unit_name: Option<String>,
    /// 折算基准单位（`Some(None)` = 清空 → 变成基础单位）。
    #[serde(default, deserialize_with = "double_option")]
    pub base_unit_id: Option<Option<String>>,
    /// 折算率。
    #[serde(default)]
    pub rate: Option<Decimal>,
    /// 启用状态。
    #[serde(default)]
    pub status: Option<i16>,
    /// 排序值。
    #[serde(default)]
    pub sort_order: Option<i32>,
    /// 备注（`Some(None)` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub remark: Option<Option<String>>,
}

impl UpdateUnitReq {
    /// 校验（只校验「给了的字段」）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(name) = self.unit_name.as_deref() {
            require_text(name, "unit_name", 50)?;
        }
        if let Some(status) = self.status {
            require_flag(status, "status")?;
        }
        if let Some(rate) = self.rate {
            require_positive(rate, "rate")?;
        }
        if let Some(remark) = self.remark.as_ref().and_then(|value| value.as_deref()) {
            check_optional_text(Some(remark), "remark", 2000)?;
        }
        Ok(())
    }
}

/// 单位筛选（分页）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct UnitFilter {
    /// 关键词（编码 / 名称模糊匹配）。
    pub keyword: Option<String>,
    /// 启用状态。
    pub status: Option<i16>,
    /// 页码（从 1 开始）。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl Default for UnitFilter {
    fn default() -> Self {
        Self {
            keyword: None,
            status: None,
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

/// 单位列表卡片。
#[derive(Debug, Clone, Serialize)]
pub struct UnitCard {
    /// 主键。
    pub id: String,
    /// 编码。
    pub unit_code: String,
    /// 名称。
    pub unit_name: String,
    /// 是否基础单位。
    pub is_base: bool,
    /// 折算率（字符串形态，前端按需格式化；`Decimal` 默认以字符串序列化，不经过 f64）。
    pub rate: Decimal,
    /// 启用状态。
    pub status: i16,
    /// 排序值。
    pub sort_order: i32,
}

impl From<&UnitRow> for UnitCard {
    fn from(row: &UnitRow) -> Self {
        Self {
            id: row.id.to_string(),
            unit_code: row.unit_code.clone(),
            unit_name: row.unit_name.clone(),
            is_base: row.is_base == 1,
            rate: row.rate,
            status: row.status,
            sort_order: row.sort_order,
        }
    }
}

/// 单位详情。
#[derive(Debug, Clone, Serialize)]
pub struct UnitRes {
    /// 主键。
    pub id: String,
    /// 编码。
    pub unit_code: String,
    /// 名称。
    pub unit_name: String,
    /// 是否基础单位。
    pub is_base: bool,
    /// 折算基准单位。
    pub base_unit_id: Option<String>,
    /// 折算率。
    pub rate: Decimal,
    /// 启用状态。
    pub status: i16,
    /// 排序值。
    pub sort_order: i32,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl From<&UnitRow> for UnitRes {
    fn from(row: &UnitRow) -> Self {
        Self {
            id: row.id.to_string(),
            unit_code: row.unit_code.clone(),
            unit_name: row.unit_name.clone(),
            is_base: row.is_base == 1,
            base_unit_id: row.base_unit_id.map(|id| id.to_string()),
            rate: row.rate,
            status: row.status,
            sort_order: row.sort_order,
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

// ==================== ② 仓库 ====================

/// 新建仓库。
///
/// `parent_id` 与 `level` 必须自洽（DB `ck_biz_warehouse_root`：`(parent_id IS NULL) = (level = 1)`）；
/// `level` 上限 3（总仓 / 分仓 / 库房）。`warehouse_path` **不由调用方给**——W1c 在建/移动时
/// 按父路径拼（`/父路径/自身 id/`），所以这里没有该字段。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateWarehouseReq {
    /// 编码（`VARCHAR(40)`，未删除内唯一）。
    pub warehouse_code: String,
    /// 名称（`VARCHAR(120)`）。
    pub warehouse_name: String,
    /// 类型（缺省 `NORMAL`）。
    #[serde(default = "default_warehouse_type")]
    pub warehouse_type: String,
    /// 负责人（文本）。
    #[serde(default)]
    pub manager: Option<String>,
    /// 是否允许负库存。
    #[serde(default)]
    pub allow_negative: bool,
    /// 地址。
    #[serde(default)]
    pub address: Option<String>,
    /// 启用状态（缺省 1）。
    #[serde(default = "default_enabled")]
    pub status: i16,
    /// 排序值（缺省 0）。
    #[serde(default)]
    pub sort_order: i32,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
    /// 上级仓库（`None` = 顶级总仓）。
    #[serde(default)]
    pub parent_id: Option<String>,
    /// 层级（1–3；`parent_id` 为空时必须 1）。
    #[serde(default = "default_level")]
    pub level: i16,
}

impl CreateWarehouseReq {
    /// 校验（`400 + 10001`）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        require_text(&self.warehouse_code, "warehouse_code", 40)?;
        require_text(&self.warehouse_name, "warehouse_name", 120)?;
        require_one_of(&self.warehouse_type, &WAREHOUSE_TYPES, "warehouse_type")?;
        require_flag(self.status, "status")?;
        check_optional_text(self.manager.as_deref(), "manager", 50)?;
        check_optional_text(self.address.as_deref(), "address", 255)?;
        if !(1..=3).contains(&self.level) {
            return Err(bad_request(format!(
                "level 只能是 1（总仓）/ 2（分仓）/ 3（库房），当前 {}",
                self.level
            )));
        }
        match (self.parent_id.is_some(), self.level) {
            (false, 1) | (true, 2..=3) => Ok(()),
            (false, _) => Err(bad_request(
                "顶级仓库（parent_id 为空）的 level 必须是 1".to_string(),
            )),
            (true, _) => Err(bad_request(
                "有上级仓库时 level 必须是 2 或 3（总仓没有上级）".to_string(),
            )),
        }
    }
}

/// 编辑仓库（字段缺席 = 不改；层级/路径由 W1c 按父节点重算）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateWarehouseReq {
    /// 名称。
    #[serde(default)]
    pub warehouse_name: Option<String>,
    /// 类型。
    #[serde(default)]
    pub warehouse_type: Option<String>,
    /// 负责人（`Some(None)` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub manager: Option<Option<String>>,
    /// 是否允许负库存。
    #[serde(default)]
    pub allow_negative: Option<bool>,
    /// 地址（`Some(None)` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub address: Option<Option<String>>,
    /// 启用状态。
    #[serde(default)]
    pub status: Option<i16>,
    /// 排序值。
    #[serde(default)]
    pub sort_order: Option<i32>,
    /// 备注（`Some(None)` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub remark: Option<Option<String>>,
}

impl UpdateWarehouseReq {
    /// 校验（只校验「给了的字段」）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(name) = self.warehouse_name.as_deref() {
            require_text(name, "warehouse_name", 120)?;
        }
        if let Some(warehouse_type) = self.warehouse_type.as_deref() {
            require_one_of(warehouse_type, &WAREHOUSE_TYPES, "warehouse_type")?;
        }
        if let Some(status) = self.status {
            require_flag(status, "status")?;
        }
        if let Some(manager) = self.manager.as_ref().and_then(|value| value.as_deref()) {
            check_optional_text(Some(manager), "manager", 50)?;
        }
        if let Some(address) = self.address.as_ref().and_then(|value| value.as_deref()) {
            check_optional_text(Some(address), "address", 255)?;
        }
        Ok(())
    }
}

/// 仓库筛选（分页）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WarehouseFilter {
    /// 关键词（编码 / 名称模糊匹配）。
    pub keyword: Option<String>,
    /// 类型。
    pub warehouse_type: Option<String>,
    /// 层级（1–3）。
    pub level: Option<i16>,
    /// 直接上级。
    pub parent_id: Option<String>,
    /// 启用状态。
    pub status: Option<i16>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl Default for WarehouseFilter {
    fn default() -> Self {
        Self {
            keyword: None,
            warehouse_type: None,
            level: None,
            parent_id: None,
            status: None,
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

/// 仓库列表卡片（含派生字段 `child_count`）。
#[derive(Debug, Clone, Serialize)]
pub struct WarehouseCard {
    /// 主键。
    pub id: String,
    /// 编码。
    pub warehouse_code: String,
    /// 名称。
    pub warehouse_name: String,
    /// 类型。
    pub warehouse_type: String,
    /// 层级。
    pub level: i16,
    /// 上级。
    pub parent_id: Option<String>,
    /// 是否叶子。
    pub is_leaf: bool,
    /// 启用状态。
    pub status: i16,
    /// 排序值。
    pub sort_order: i32,
    /// **派生字段**：直接子节点个数（由 `warehouse_child_counts` 一次算出，不是库列）。
    pub child_count: i64,
}

impl WarehouseCard {
    /// 从行 + 子节点数构造（列表接口用）。
    pub fn from_row(row: &WarehouseRow, child_count: i64) -> Self {
        Self {
            id: row.id.to_string(),
            warehouse_code: row.warehouse_code.clone(),
            warehouse_name: row.warehouse_name.clone(),
            warehouse_type: row.warehouse_type.clone(),
            level: row.level,
            parent_id: row.parent_id.map(|id| id.to_string()),
            is_leaf: row.is_leaf == 1,
            status: row.status,
            sort_order: row.sort_order,
            child_count,
        }
    }
}

impl From<&WarehouseRow> for WarehouseCard {
    /// 不查子节点数（树接口用；树里 `is_leaf` 已能判断，避免 N+1）。
    fn from(row: &WarehouseRow) -> Self {
        Self::from_row(row, 0)
    }
}

/// 仓库详情。
#[derive(Debug, Clone, Serialize)]
pub struct WarehouseRes {
    /// 主键。
    pub id: String,
    /// 编码。
    pub warehouse_code: String,
    /// 名称。
    pub warehouse_name: String,
    /// 类型。
    pub warehouse_type: String,
    /// 负责人。
    pub manager: Option<String>,
    /// 是否允许负库存。
    pub allow_negative: bool,
    /// 地址。
    pub address: Option<String>,
    /// 启用状态。
    pub status: i16,
    /// 排序值。
    pub sort_order: i32,
    /// 备注。
    pub remark: Option<String>,
    /// 上级。
    pub parent_id: Option<String>,
    /// 层级。
    pub level: i16,
    /// 物化路径（`/uuid/uuid/`）。
    pub warehouse_path: String,
    /// 是否叶子。
    pub is_leaf: bool,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl From<&WarehouseRow> for WarehouseRes {
    fn from(row: &WarehouseRow) -> Self {
        Self {
            id: row.id.to_string(),
            warehouse_code: row.warehouse_code.clone(),
            warehouse_name: row.warehouse_name.clone(),
            warehouse_type: row.warehouse_type.clone(),
            manager: row.manager.clone(),
            allow_negative: row.allow_negative == 1,
            address: row.address.clone(),
            status: row.status,
            sort_order: row.sort_order,
            remark: row.remark.clone(),
            parent_id: row.parent_id.map(|id| id.to_string()),
            level: row.level,
            warehouse_path: row.warehouse_path.clone(),
            is_leaf: row.is_leaf == 1,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

// ==================== ③ 库位 ====================

/// 新建库位（编码仓库内唯一，`CHECK` 6 类库位类型）。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateLocationReq {
    /// 库位编码（`VARCHAR(40)`，同一仓库内唯一）。
    pub location_code: String,
    /// 所属仓库。
    pub warehouse_id: String,
    /// 库位名称。
    #[serde(default)]
    pub location_name: Option<String>,
    /// 类型（缺省 `STORAGE`）。
    #[serde(default = "default_location_type")]
    pub location_type: String,
    /// 是否仓库默认库位（每仓最多一个）。
    #[serde(default)]
    pub is_default: bool,
    /// 容量。
    #[serde(default)]
    pub capacity: Option<Decimal>,
    /// 启用状态（缺省 1）。
    #[serde(default = "default_enabled")]
    pub status: i16,
    /// 排序值。
    #[serde(default)]
    pub sort_order: i32,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

impl CreateLocationReq {
    /// 校验（`400 + 10001`）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        require_text(&self.location_code, "location_code", 40)?;
        require_text(&self.warehouse_id, "warehouse_id", 36)?;
        require_one_of(&self.location_type, &LOCATION_TYPES, "location_type")?;
        require_flag(self.status, "status")?;
        check_optional_text(self.location_name.as_deref(), "location_name", 120)?;
        if let Some(capacity) = self.capacity {
            require_non_negative(capacity, "capacity")?;
        }
        Ok(())
    }
}

/// 编辑库位（字段缺席 = 不改；**不允许换仓库**——换仓库等于换身份，应重建）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateLocationReq {
    /// 名称。
    #[serde(default, deserialize_with = "double_option")]
    pub location_name: Option<Option<String>>,
    /// 类型。
    #[serde(default)]
    pub location_type: Option<String>,
    /// 是否默认库位。
    #[serde(default)]
    pub is_default: Option<bool>,
    /// 容量（`Some(None)` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub capacity: Option<Option<Decimal>>,
    /// 启用状态。
    #[serde(default)]
    pub status: Option<i16>,
    /// 排序值。
    #[serde(default)]
    pub sort_order: Option<i32>,
    /// 备注。
    #[serde(default, deserialize_with = "double_option")]
    pub remark: Option<Option<String>>,
}

impl UpdateLocationReq {
    /// 校验。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(location_type) = self.location_type.as_deref() {
            require_one_of(location_type, &LOCATION_TYPES, "location_type")?;
        }
        if let Some(status) = self.status {
            require_flag(status, "status")?;
        }
        if let Some(Some(capacity)) = self.capacity {
            require_non_negative(capacity, "capacity")?;
        }
        Ok(())
    }
}

/// 库位筛选（分页）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LocationFilter {
    /// 关键词（编码 / 名称模糊匹配）。
    pub keyword: Option<String>,
    /// 所属仓库。
    pub warehouse_id: Option<String>,
    /// 类型。
    pub location_type: Option<String>,
    /// 启用状态。
    pub status: Option<i16>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl Default for LocationFilter {
    fn default() -> Self {
        Self {
            keyword: None,
            warehouse_id: None,
            location_type: None,
            status: None,
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

/// 库位列表卡片。
#[derive(Debug, Clone, Serialize)]
pub struct LocationCard {
    /// 主键。
    pub id: String,
    /// 库位编码。
    pub location_code: String,
    /// 库位名称。
    pub location_name: Option<String>,
    /// 所属仓库。
    pub warehouse_id: String,
    /// 类型。
    pub location_type: String,
    /// 是否默认库位。
    pub is_default: bool,
    /// 启用状态。
    pub status: i16,
    /// 排序值。
    pub sort_order: i32,
}

impl From<&LocationRow> for LocationCard {
    fn from(row: &LocationRow) -> Self {
        Self {
            id: row.id.to_string(),
            location_code: row.location_code.clone(),
            location_name: row.location_name.clone(),
            warehouse_id: row.warehouse_id.to_string(),
            location_type: row.location_type.clone(),
            is_default: row.is_default == 1,
            status: row.status,
            sort_order: row.sort_order,
        }
    }
}

/// 库位详情。
#[derive(Debug, Clone, Serialize)]
pub struct LocationRes {
    /// 主键。
    pub id: String,
    /// 库位编码。
    pub location_code: String,
    /// 库位名称。
    pub location_name: Option<String>,
    /// 所属仓库。
    pub warehouse_id: String,
    /// 类型。
    pub location_type: String,
    /// 是否默认库位。
    pub is_default: bool,
    /// 容量。
    pub capacity: Option<Decimal>,
    /// 启用状态。
    pub status: i16,
    /// 排序值。
    pub sort_order: i32,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl From<&LocationRow> for LocationRes {
    fn from(row: &LocationRow) -> Self {
        Self {
            id: row.id.to_string(),
            location_code: row.location_code.clone(),
            location_name: row.location_name.clone(),
            warehouse_id: row.warehouse_id.to_string(),
            location_type: row.location_type.clone(),
            is_default: row.is_default == 1,
            capacity: row.capacity,
            status: row.status,
            sort_order: row.sort_order,
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

// ==================== 缺省值小工具 ====================

fn default_true() -> bool {
    true
}

fn default_enabled() -> i16 {
    1
}

fn default_level() -> i16 {
    1
}

fn default_warehouse_type() -> String {
    "NORMAL".to_string()
}

fn default_location_type() -> String {
    "STORAGE".to_string()
}

fn default_material_type() -> String {
    "RAW".to_string()
}

fn default_costing_method() -> String {
    "MOVING_AVG".to_string()
}

// ==================== ④ 物料 SKU ====================

/// 新建物料。
///
/// `serial_required` 是 V0138 加的一物一码开关：**W2b 补口**（W1c 落地时该列还不存在，
/// 接口没接这一列 → 物料接口无法打开一物一码，序列号功能就无从启用）。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateMaterialReq {
    /// 物料编码（`VARCHAR(40)`，未删除内唯一）。
    pub material_code: String,
    /// 物料名称（`VARCHAR(120)`）。
    pub material_name: String,
    /// 规格型号。
    #[serde(default)]
    pub spec: Option<String>,
    /// 基础计量单位。
    pub unit_id: String,
    /// 物料类型（缺省 `RAW`）。
    #[serde(default = "default_material_type")]
    pub material_type: String,
    /// 默认仓库。
    #[serde(default)]
    pub default_warehouse_id: Option<String>,
    /// 安全库存（缺省 0）。
    #[serde(default)]
    pub safety_stock: Option<Decimal>,
    /// 计价方式（缺省 `MOVING_AVG`）。
    #[serde(default = "default_costing_method")]
    pub costing_method: String,
    /// 存货科目。
    #[serde(default)]
    pub inventory_account_id: Option<String>,
    /// 税率（`0`–`1`，缺省 0）。
    #[serde(default)]
    pub tax_rate: Option<Decimal>,
    /// 启用状态（缺省 1）。
    #[serde(default = "default_enabled")]
    pub status: i16,
    /// 是否强制一物一码（缺省 `false`；V0138 的列）。
    #[serde(default)]
    pub serial_required: Option<bool>,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

impl CreateMaterialReq {
    /// 校验（`400 + 10001`）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        require_text(&self.material_code, "material_code", 40)?;
        require_text(&self.material_name, "material_name", 120)?;
        require_text(&self.unit_id, "unit_id", 36)?;
        require_one_of(&self.material_type, &MATERIAL_TYPES, "material_type")?;
        require_one_of(&self.costing_method, &COSTING_METHODS, "costing_method")?;
        require_flag(self.status, "status")?;
        check_optional_text(self.spec.as_deref(), "spec", 200)?;
        if let Some(safety_stock) = self.safety_stock {
            require_non_negative(safety_stock, "safety_stock")?;
        }
        if let Some(tax_rate) = self.tax_rate {
            require_tax_rate(tax_rate)?;
        }
        Ok(())
    }
}

/// 编辑物料。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateMaterialReq {
    /// 名称。
    #[serde(default)]
    pub material_name: Option<String>,
    /// 规格（`Some(None)` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub spec: Option<Option<String>>,
    /// 基础单位。
    #[serde(default)]
    pub unit_id: Option<String>,
    /// 类型。
    #[serde(default)]
    pub material_type: Option<String>,
    /// 默认仓库（`Some(None)` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub default_warehouse_id: Option<Option<String>>,
    /// 安全库存。
    #[serde(default)]
    pub safety_stock: Option<Decimal>,
    /// 计价方式。
    #[serde(default)]
    pub costing_method: Option<String>,
    /// 存货科目（`Some(None)` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub inventory_account_id: Option<Option<String>>,
    /// 税率。
    #[serde(default)]
    pub tax_rate: Option<Decimal>,
    /// 启用状态。
    #[serde(default)]
    pub status: Option<i16>,
    /// 一物一码开关（缺席 = 不改；V0138 的列）。
    #[serde(default)]
    pub serial_required: Option<bool>,
    /// 备注（`Some(None)` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub remark: Option<Option<String>>,
}

impl UpdateMaterialReq {
    /// 校验。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(name) = self.material_name.as_deref() {
            require_text(name, "material_name", 120)?;
        }
        if let Some(material_type) = self.material_type.as_deref() {
            require_one_of(material_type, &MATERIAL_TYPES, "material_type")?;
        }
        if let Some(costing_method) = self.costing_method.as_deref() {
            require_one_of(costing_method, &COSTING_METHODS, "costing_method")?;
        }
        if let Some(status) = self.status {
            require_flag(status, "status")?;
        }
        if let Some(safety_stock) = self.safety_stock {
            require_non_negative(safety_stock, "safety_stock")?;
        }
        if let Some(tax_rate) = self.tax_rate {
            require_tax_rate(tax_rate)?;
        }
        if let Some(spec) = self.spec.as_ref().and_then(|value| value.as_deref()) {
            check_optional_text(Some(spec), "spec", 200)?;
        }
        Ok(())
    }
}

/// 物料筛选（分页）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MaterialFilter {
    /// 关键词（编码 / 名称 / 规格模糊匹配）。
    pub keyword: Option<String>,
    /// 物料类型。
    pub material_type: Option<String>,
    /// 基础单位。
    pub unit_id: Option<String>,
    /// 默认仓库。
    pub default_warehouse_id: Option<String>,
    /// 启用状态。
    pub status: Option<i16>,
    /// 一物一码（**V0138 才有的列**；列不存在时 `material_page` 显式报错，见 `SERIAL_COLUMN_MISSING`）。
    pub serial_required: Option<bool>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl Default for MaterialFilter {
    fn default() -> Self {
        Self {
            keyword: None,
            material_type: None,
            unit_id: None,
            default_warehouse_id: None,
            status: None,
            serial_required: None,
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

/// 物料列表卡片。
#[derive(Debug, Clone, Serialize)]
pub struct MaterialCard {
    /// 主键。
    pub id: String,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 规格。
    pub spec: Option<String>,
    /// 类型。
    pub material_type: String,
    /// 基础单位。
    pub unit_id: String,
    /// 安全库存。
    pub safety_stock: Decimal,
    /// 启用状态。
    pub status: i16,
}

impl From<&MaterialRow> for MaterialCard {
    fn from(row: &MaterialRow) -> Self {
        Self {
            id: row.id.to_string(),
            material_code: row.material_code.clone(),
            material_name: row.material_name.clone(),
            spec: row.spec.clone(),
            material_type: row.material_type.clone(),
            unit_id: row.unit_id.to_string(),
            safety_stock: row.safety_stock,
            status: row.status,
        }
    }
}

/// 物料详情。
#[derive(Debug, Clone, Serialize)]
pub struct MaterialRes {
    /// 主键。
    pub id: String,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 规格。
    pub spec: Option<String>,
    /// 基础单位。
    pub unit_id: String,
    /// 类型。
    pub material_type: String,
    /// 默认仓库。
    pub default_warehouse_id: Option<String>,
    /// 安全库存。
    pub safety_stock: Decimal,
    /// 计价方式。
    pub costing_method: String,
    /// 存货科目。
    pub inventory_account_id: Option<String>,
    /// 税率。
    pub tax_rate: Decimal,
    /// 启用状态。
    pub status: i16,
    /// 是否强制一物一码（V0138 的列；前端据此显示「一物一码」标签与生成入口）。
    pub serial_required: bool,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl From<&MaterialRow> for MaterialRes {
    fn from(row: &MaterialRow) -> Self {
        Self {
            id: row.id.to_string(),
            material_code: row.material_code.clone(),
            material_name: row.material_name.clone(),
            spec: row.spec.clone(),
            unit_id: row.unit_id.to_string(),
            material_type: row.material_type.clone(),
            default_warehouse_id: row.default_warehouse_id.map(|id| id.to_string()),
            safety_stock: row.safety_stock,
            costing_method: row.costing_method.clone(),
            inventory_account_id: row.inventory_account_id.map(|id| id.to_string()),
            tax_rate: row.tax_rate,
            status: row.status,
            serial_required: row.serial_required,
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

// ==================== ⑤ 供应商 ====================

/// 新建供应商。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateSupplierReq {
    /// 编码（`VARCHAR(40)`，未删除内唯一）。
    pub supplier_code: String,
    /// 名称（`VARCHAR(200)`）。
    pub supplier_name: String,
    /// 简称。
    #[serde(default)]
    pub short_name: Option<String>,
    /// 联系人。
    #[serde(default)]
    pub contact_person: Option<String>,
    /// 联系电话。
    #[serde(default)]
    pub contact_phone: Option<String>,
    /// 邮箱。
    #[serde(default)]
    pub email: Option<String>,
    /// 纳税人识别号。
    #[serde(default)]
    pub tax_no: Option<String>,
    /// 地址。
    #[serde(default)]
    pub address: Option<String>,
    /// 账期天数（`>= 0`）。
    #[serde(default)]
    pub credit_days: i32,
    /// 信用额度（`>= 0`）。
    #[serde(default)]
    pub credit_limit: Option<Decimal>,
    /// 应付科目。
    #[serde(default)]
    pub payable_account_id: Option<String>,
    /// 开票抬头。
    #[serde(default)]
    pub invoice_title: Option<String>,
    /// 开户行。
    #[serde(default)]
    pub invoice_bank_name: Option<String>,
    /// 银行账号。
    #[serde(default)]
    pub invoice_bank_account: Option<String>,
    /// 启用状态（缺省 1）。
    #[serde(default = "default_enabled")]
    pub status: i16,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

impl CreateSupplierReq {
    /// 校验（`400 + 10001`）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        require_text(&self.supplier_code, "supplier_code", 40)?;
        require_text(&self.supplier_name, "supplier_name", 200)?;
        require_flag(self.status, "status")?;
        check_optional_text(self.short_name.as_deref(), "short_name", 100)?;
        check_optional_text(self.contact_person.as_deref(), "contact_person", 50)?;
        check_optional_text(self.contact_phone.as_deref(), "contact_phone", 30)?;
        check_optional_text(self.email.as_deref(), "email", 120)?;
        check_optional_text(self.tax_no.as_deref(), "tax_no", 50)?;
        check_optional_text(self.address.as_deref(), "address", 255)?;
        check_optional_text(self.invoice_title.as_deref(), "invoice_title", 200)?;
        check_optional_text(self.invoice_bank_name.as_deref(), "invoice_bank_name", 200)?;
        check_optional_text(
            self.invoice_bank_account.as_deref(),
            "invoice_bank_account",
            64,
        )?;
        if self.credit_days < 0 {
            return Err(bad_request(format!(
                "credit_days 不能为负数（当前 {}）",
                self.credit_days
            )));
        }
        if let Some(credit_limit) = self.credit_limit {
            require_non_negative(credit_limit, "credit_limit")?;
        }
        Ok(())
    }
}

/// 编辑供应商。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateSupplierReq {
    /// 名称。
    #[serde(default)]
    pub supplier_name: Option<String>,
    /// 简称。
    #[serde(default, deserialize_with = "double_option")]
    pub short_name: Option<Option<String>>,
    /// 联系人。
    #[serde(default, deserialize_with = "double_option")]
    pub contact_person: Option<Option<String>>,
    /// 联系电话。
    #[serde(default, deserialize_with = "double_option")]
    pub contact_phone: Option<Option<String>>,
    /// 邮箱。
    #[serde(default, deserialize_with = "double_option")]
    pub email: Option<Option<String>>,
    /// 纳税人识别号。
    #[serde(default, deserialize_with = "double_option")]
    pub tax_no: Option<Option<String>>,
    /// 地址。
    #[serde(default, deserialize_with = "double_option")]
    pub address: Option<Option<String>>,
    /// 账期天数。
    #[serde(default)]
    pub credit_days: Option<i32>,
    /// 信用额度。
    #[serde(default)]
    pub credit_limit: Option<Decimal>,
    /// 应付科目。
    #[serde(default, deserialize_with = "double_option")]
    pub payable_account_id: Option<Option<String>>,
    /// 开票抬头。
    #[serde(default, deserialize_with = "double_option")]
    pub invoice_title: Option<Option<String>>,
    /// 开户行。
    #[serde(default, deserialize_with = "double_option")]
    pub invoice_bank_name: Option<Option<String>>,
    /// 银行账号。
    #[serde(default, deserialize_with = "double_option")]
    pub invoice_bank_account: Option<Option<String>>,
    /// 启用状态。
    #[serde(default)]
    pub status: Option<i16>,
    /// 备注。
    #[serde(default, deserialize_with = "double_option")]
    pub remark: Option<Option<String>>,
}

impl UpdateSupplierReq {
    /// 校验。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(name) = self.supplier_name.as_deref() {
            require_text(name, "supplier_name", 200)?;
        }
        if let Some(status) = self.status {
            require_flag(status, "status")?;
        }
        if let Some(credit_days) = self.credit_days
            && credit_days < 0
        {
            return Err(bad_request(format!(
                "credit_days 不能为负数（当前 {credit_days}）"
            )));
        }
        if let Some(credit_limit) = self.credit_limit {
            require_non_negative(credit_limit, "credit_limit")?;
        }
        Ok(())
    }
}

/// 供应商筛选（分页）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SupplierFilter {
    /// 关键词（编码 / 名称 / 简称）。
    pub keyword: Option<String>,
    /// 启用状态。
    pub status: Option<i16>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl Default for SupplierFilter {
    fn default() -> Self {
        Self {
            keyword: None,
            status: None,
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

/// 供应商列表卡片。
#[derive(Debug, Clone, Serialize)]
pub struct SupplierCard {
    /// 主键。
    pub id: String,
    /// 编码。
    pub supplier_code: String,
    /// 名称。
    pub supplier_name: String,
    /// 简称。
    pub short_name: Option<String>,
    /// 联系人。
    pub contact_person: Option<String>,
    /// 联系电话。
    pub contact_phone: Option<String>,
    /// 账期天数。
    pub credit_days: i32,
    /// 信用额度。
    pub credit_limit: Decimal,
    /// 启用状态。
    pub status: i16,
}

impl From<&SupplierRow> for SupplierCard {
    fn from(row: &SupplierRow) -> Self {
        Self {
            id: row.id.to_string(),
            supplier_code: row.supplier_code.clone(),
            supplier_name: row.supplier_name.clone(),
            short_name: row.short_name.clone(),
            contact_person: row.contact_person.clone(),
            contact_phone: row.contact_phone.clone(),
            credit_days: row.credit_days,
            credit_limit: row.credit_limit,
            status: row.status,
        }
    }
}

/// 供应商详情。
#[derive(Debug, Clone, Serialize)]
pub struct SupplierRes {
    /// 主键。
    pub id: String,
    /// 编码。
    pub supplier_code: String,
    /// 名称。
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
    /// 信用额度。
    pub credit_limit: Decimal,
    /// 应付科目。
    pub payable_account_id: Option<String>,
    /// 开票抬头。
    pub invoice_title: Option<String>,
    /// 开户行。
    pub invoice_bank_name: Option<String>,
    /// 银行账号。
    pub invoice_bank_account: Option<String>,
    /// 启用状态。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl From<&SupplierRow> for SupplierRes {
    fn from(row: &SupplierRow) -> Self {
        Self {
            id: row.id.to_string(),
            supplier_code: row.supplier_code.clone(),
            supplier_name: row.supplier_name.clone(),
            short_name: row.short_name.clone(),
            contact_person: row.contact_person.clone(),
            contact_phone: row.contact_phone.clone(),
            email: row.email.clone(),
            tax_no: row.tax_no.clone(),
            address: row.address.clone(),
            credit_days: row.credit_days,
            credit_limit: row.credit_limit,
            payable_account_id: row.payable_account_id.map(|id| id.to_string()),
            invoice_title: row.invoice_title.clone(),
            invoice_bank_name: row.invoice_bank_name.clone(),
            invoice_bank_account: row.invoice_bank_account.clone(),
            status: row.status,
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

// ==================== ⑥ 会计科目 ====================

/// 新建会计科目。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateAccountReq {
    /// 科目编码（`VARCHAR(30)`，未删除内唯一）。
    pub account_code: String,
    /// 科目名称（`VARCHAR(120)`）。
    pub account_name: String,
    /// 上级科目（`None` = 一级）。
    #[serde(default)]
    pub parent_id: Option<String>,
    /// 级次（`>= 1`，缺省 1）。
    #[serde(default = "default_level")]
    pub level: i16,
    /// 科目类别。
    pub account_category: String,
    /// 余额方向（`DEBIT` / `CREDIT`）。
    pub balance_direction: String,
    /// 是否末级（缺省 1）。
    #[serde(default = "default_enabled")]
    pub is_leaf: i16,
    /// 辅助核算维度。
    #[serde(default)]
    pub aux_dimensions: Option<String>,
    /// 启用状态（缺省 1）。
    #[serde(default = "default_enabled")]
    pub status: i16,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

impl CreateAccountReq {
    /// 校验（`400 + 10001`）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        require_text(&self.account_code, "account_code", 30)?;
        require_text(&self.account_name, "account_name", 120)?;
        require_one_of(
            &self.account_category,
            &ACCOUNT_CATEGORIES,
            "account_category",
        )?;
        require_one_of(
            &self.balance_direction,
            &BALANCE_DIRECTIONS,
            "balance_direction",
        )?;
        require_flag(self.status, "status")?;
        require_flag(self.is_leaf, "is_leaf")?;
        check_optional_text(self.aux_dimensions.as_deref(), "aux_dimensions", 120)?;
        if self.level < 1 {
            return Err(bad_request(format!(
                "level 必须 >= 1（当前 {}）",
                self.level
            )));
        }
        match (self.parent_id.is_some(), self.level) {
            (false, 1) | (true, 2..=i16::MAX) => Ok(()),
            (false, _) => Err(bad_request(
                "一级科目（parent_id 为空）的 level 必须是 1".to_string(),
            )),
            (true, _) => Err(bad_request("有上级科目时 level 必须 >= 2".to_string())),
        }
    }
}

/// 编辑会计科目。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateAccountReq {
    /// 名称。
    #[serde(default)]
    pub account_name: Option<String>,
    /// 是否末级。
    #[serde(default)]
    pub is_leaf: Option<i16>,
    /// 辅助核算维度。
    #[serde(default, deserialize_with = "double_option")]
    pub aux_dimensions: Option<Option<String>>,
    /// 启用状态。
    #[serde(default)]
    pub status: Option<i16>,
    /// 备注。
    #[serde(default, deserialize_with = "double_option")]
    pub remark: Option<Option<String>>,
}

impl UpdateAccountReq {
    /// 校验。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(name) = self.account_name.as_deref() {
            require_text(name, "account_name", 120)?;
        }
        if let Some(is_leaf) = self.is_leaf {
            require_flag(is_leaf, "is_leaf")?;
        }
        if let Some(status) = self.status {
            require_flag(status, "status")?;
        }
        Ok(())
    }
}

/// 科目筛选（分页）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AccountFilter {
    /// 关键词（编码 / 名称模糊匹配）。
    pub keyword: Option<String>,
    /// 科目类别。
    pub account_category: Option<String>,
    /// 只看末级 / 只看非末级。
    pub is_leaf: Option<bool>,
    /// 直接上级。
    pub parent_id: Option<String>,
    /// 启用状态。
    pub status: Option<i16>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl Default for AccountFilter {
    fn default() -> Self {
        Self {
            keyword: None,
            account_category: None,
            is_leaf: None,
            parent_id: None,
            status: None,
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

/// 科目列表卡片。
#[derive(Debug, Clone, Serialize)]
pub struct AccountCard {
    /// 主键。
    pub id: String,
    /// 编码。
    pub account_code: String,
    /// 名称。
    pub account_name: String,
    /// 上级。
    pub parent_id: Option<String>,
    /// 级次。
    pub level: i16,
    /// 类别。
    pub account_category: String,
    /// 余额方向。
    pub balance_direction: String,
    /// 是否末级。
    pub is_leaf: bool,
    /// 启用状态。
    pub status: i16,
}

impl From<&AccountRow> for AccountCard {
    fn from(row: &AccountRow) -> Self {
        Self {
            id: row.id.to_string(),
            account_code: row.account_code.clone(),
            account_name: row.account_name.clone(),
            parent_id: row.parent_id.map(|id| id.to_string()),
            level: row.level,
            account_category: row.account_category.clone(),
            balance_direction: row.balance_direction.clone(),
            is_leaf: row.is_leaf == 1,
            status: row.status,
        }
    }
}

/// 科目详情。
#[derive(Debug, Clone, Serialize)]
pub struct AccountRes {
    /// 主键。
    pub id: String,
    /// 编码。
    pub account_code: String,
    /// 名称。
    pub account_name: String,
    /// 上级。
    pub parent_id: Option<String>,
    /// 级次。
    pub level: i16,
    /// 类别。
    pub account_category: String,
    /// 余额方向。
    pub balance_direction: String,
    /// 是否末级。
    pub is_leaf: bool,
    /// 辅助核算维度。
    pub aux_dimensions: Option<String>,
    /// 启用状态。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl From<&AccountRow> for AccountRes {
    fn from(row: &AccountRow) -> Self {
        Self {
            id: row.id.to_string(),
            account_code: row.account_code.clone(),
            account_name: row.account_name.clone(),
            parent_id: row.parent_id.map(|id| id.to_string()),
            level: row.level,
            account_category: row.account_category.clone(),
            balance_direction: row.balance_direction.clone(),
            is_leaf: row.is_leaf == 1,
            aux_dimensions: row.aux_dimensions.clone(),
            status: row.status,
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

// ==================== ⑦ 会计期间 ====================

/// 新建会计期间。
///
/// `period_code` 由 `(year, month)` 推导（`YYYY-MM`），**不由调用方给**：
/// 让调用方自由填会出现「2026-10 却是 11 月」的对不上号。
#[derive(Debug, Clone, Deserialize)]
pub struct CreatePeriodReq {
    /// 年份。
    pub period_year: i16,
    /// 月份（1–12）。
    pub period_month: i16,
    /// 开始日。
    pub start_date: NaiveDate,
    /// 结束日。
    pub end_date: NaiveDate,
    /// 状态（缺省 `NOT_OPENED`）。
    #[serde(default = "default_period_status")]
    pub status: String,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

impl CreatePeriodReq {
    /// 校验（`400 + 10001`）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if !(1..=12).contains(&self.period_month) {
            return Err(bad_request(format!(
                "period_month 只能是 1–12（当前 {}）",
                self.period_month
            )));
        }
        if self.start_date > self.end_date {
            return Err(bad_request(format!(
                "start_date（{}）不能晚于 end_date（{}）",
                self.start_date, self.end_date
            )));
        }
        require_one_of(&self.status, &PERIOD_STATUSES, "status")
    }

    /// 由 `(year, month)` 推导的期间编码（`YYYY-MM`，与 `VARCHAR(7)` 等长）。
    pub fn period_code(&self) -> String {
        format!("{:04}-{:02}", self.period_year, self.period_month)
    }
}

/// 编辑会计期间（只允许改状态与备注：日期与年月是期间的**身份**）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdatePeriodReq {
    /// 状态。
    #[serde(default)]
    pub status: Option<String>,
    /// 备注。
    #[serde(default, deserialize_with = "double_option")]
    pub remark: Option<Option<String>>,
}

impl UpdatePeriodReq {
    /// 校验。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(status) = self.status.as_deref() {
            require_one_of(status, &PERIOD_STATUSES, "status")?;
        }
        Ok(())
    }
}

/// 期间筛选（分页）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PeriodFilter {
    /// 年份。
    pub period_year: Option<i16>,
    /// 状态。
    pub status: Option<String>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl Default for PeriodFilter {
    fn default() -> Self {
        Self {
            period_year: None,
            status: None,
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

/// 期间列表卡片。
#[derive(Debug, Clone, Serialize)]
pub struct PeriodCard {
    /// 主键。
    pub id: String,
    /// 年份。
    pub period_year: i16,
    /// 月份。
    pub period_month: i16,
    /// 期间编码（`YYYY-MM`）。
    pub period_code: String,
    /// 开始日。
    pub start_date: NaiveDate,
    /// 结束日。
    pub end_date: NaiveDate,
    /// 状态。
    pub status: String,
    /// 结账时间。
    pub closed_at: Option<DateTime<Utc>>,
}

impl From<&PeriodRow> for PeriodCard {
    fn from(row: &PeriodRow) -> Self {
        Self {
            id: row.id.to_string(),
            period_year: row.period_year,
            period_month: row.period_month,
            period_code: row.period_code.clone(),
            start_date: row.start_date,
            end_date: row.end_date,
            status: row.status.clone(),
            closed_at: row.closed_at,
        }
    }
}

/// 期间详情。
#[derive(Debug, Clone, Serialize)]
pub struct PeriodRes {
    /// 主键。
    pub id: String,
    /// 年份。
    pub period_year: i16,
    /// 月份。
    pub period_month: i16,
    /// 期间编码。
    pub period_code: String,
    /// 开始日。
    pub start_date: NaiveDate,
    /// 结束日。
    pub end_date: NaiveDate,
    /// 状态。
    pub status: String,
    /// 结账时间。
    pub closed_at: Option<DateTime<Utc>>,
    /// 结账人。
    pub closed_by: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl From<&PeriodRow> for PeriodRes {
    fn from(row: &PeriodRow) -> Self {
        Self {
            id: row.id.to_string(),
            period_year: row.period_year,
            period_month: row.period_month,
            period_code: row.period_code.clone(),
            start_date: row.start_date,
            end_date: row.end_date,
            status: row.status.clone(),
            closed_at: row.closed_at,
            closed_by: row.closed_by.map(|id| id.to_string()),
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

fn default_period_status() -> String {
    "NOT_OPENED".to_string()
}

// ==================== ⑧ 单号序列 ====================

/// 新建单号序列。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateSeqReq {
    /// 序列类型（`VARCHAR(32)`，未删除内唯一）。
    pub seq_type: String,
    /// 名称（`VARCHAR(50)`）。
    pub seq_name: String,
    /// 前缀（`VARCHAR(12)`）。
    pub prefix: String,
    /// 日期段格式（缺省 `yyyyMMdd`）。
    #[serde(default = "default_date_pattern")]
    pub date_pattern: String,
    /// 步长（`> 0`，缺省 1）。
    #[serde(default = "default_step")]
    pub step: i32,
    /// 分隔符（缺省 `-`）。
    #[serde(default = "default_separator")]
    pub separator: String,
    /// 启用状态（缺省 1）。
    #[serde(default = "default_enabled")]
    pub status: i16,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

impl CreateSeqReq {
    /// 校验（`400 + 10001`）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        require_text(&self.seq_type, "seq_type", 32)?;
        require_text(&self.seq_name, "seq_name", 50)?;
        require_text(&self.prefix, "prefix", 12)?;
        require_one_of(&self.date_pattern, &DATE_PATTERNS, "date_pattern")?;
        check_optional_text(Some(&self.separator), "separator", 4)?;
        if self.step <= 0 {
            return Err(bad_request(format!(
                "step 必须大于 0（当前 {}）",
                self.step
            )));
        }
        require_flag(self.status, "status")
    }
}

/// 编辑单号序列（`current_no` **不可改**：改了会重号或跳号）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateSeqReq {
    /// 名称。
    #[serde(default)]
    pub seq_name: Option<String>,
    /// 前缀。
    #[serde(default)]
    pub prefix: Option<String>,
    /// 日期段格式。
    #[serde(default)]
    pub date_pattern: Option<String>,
    /// 步长。
    #[serde(default)]
    pub step: Option<i32>,
    /// 分隔符。
    #[serde(default)]
    pub separator: Option<String>,
    /// 启用状态。
    #[serde(default)]
    pub status: Option<i16>,
    /// 备注。
    #[serde(default, deserialize_with = "double_option")]
    pub remark: Option<Option<String>>,
}

impl UpdateSeqReq {
    /// 校验。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(name) = self.seq_name.as_deref() {
            require_text(name, "seq_name", 50)?;
        }
        if let Some(prefix) = self.prefix.as_deref() {
            require_text(prefix, "prefix", 12)?;
        }
        if let Some(date_pattern) = self.date_pattern.as_deref() {
            require_one_of(date_pattern, &DATE_PATTERNS, "date_pattern")?;
        }
        if let Some(step) = self.step
            && step <= 0
        {
            return Err(bad_request(format!("step 必须大于 0（当前 {step}）")));
        }
        if let Some(status) = self.status {
            require_flag(status, "status")?;
        }
        Ok(())
    }
}

/// 序列筛选（分页）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SeqFilter {
    /// 关键词（类型 / 名称）。
    pub keyword: Option<String>,
    /// 启用状态。
    pub status: Option<i16>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl Default for SeqFilter {
    fn default() -> Self {
        Self {
            keyword: None,
            status: None,
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

/// 序列列表卡片。
#[derive(Debug, Clone, Serialize)]
pub struct SeqCard {
    /// 主键。
    pub id: String,
    /// 序列类型。
    pub seq_type: String,
    /// 名称。
    pub seq_name: String,
    /// 前缀。
    pub prefix: String,
    /// 日期段格式。
    pub date_pattern: String,
    /// 当前周期键。
    pub period_key: String,
    /// 当前序号。
    pub current_no: i64,
    /// 步长。
    pub step: i32,
    /// 启用状态。
    pub status: i16,
}

impl From<&SeqRow> for SeqCard {
    fn from(row: &SeqRow) -> Self {
        Self {
            id: row.id.to_string(),
            seq_type: row.seq_type.clone(),
            seq_name: row.seq_name.clone(),
            prefix: row.prefix.clone(),
            date_pattern: row.date_pattern.clone(),
            period_key: row.period_key.clone(),
            current_no: row.current_no,
            step: row.step,
            status: row.status,
        }
    }
}

/// 序列详情。
#[derive(Debug, Clone, Serialize)]
pub struct SeqRes {
    /// 主键。
    pub id: String,
    /// 序列类型。
    pub seq_type: String,
    /// 名称。
    pub seq_name: String,
    /// 前缀。
    pub prefix: String,
    /// 日期段格式。
    pub date_pattern: String,
    /// 当前周期键。
    pub period_key: String,
    /// 当前序号。
    pub current_no: i64,
    /// 步长。
    pub step: i32,
    /// 分隔符。
    pub separator: String,
    /// 启用状态。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl From<&SeqRow> for SeqRes {
    fn from(row: &SeqRow) -> Self {
        Self {
            id: row.id.to_string(),
            seq_type: row.seq_type.clone(),
            seq_name: row.seq_name.clone(),
            prefix: row.prefix.clone(),
            date_pattern: row.date_pattern.clone(),
            period_key: row.period_key.clone(),
            current_no: row.current_no,
            step: row.step,
            separator: row.separator.clone(),
            status: row.status,
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

fn default_date_pattern() -> String {
    "yyyyMMdd".to_string()
}

fn default_step() -> i32 {
    1
}

fn default_separator() -> String {
    "-".to_string()
}

// ==================== ⑨ 采购订单 ====================

/// 新建采购订单（主单；明细一起提交见 [`CreatePurchaseOrderItemReq`]）。
///
/// `order_no` **不由调用方给**：由 `biz_seq` 的 `PO` 序列在事务内取号（W1c 落地），
/// 避免前端自造号导致重号。`total_*` 三个合计也由明细汇总，收货前一律为 0 起步。
#[derive(Debug, Clone, Deserialize)]
pub struct CreatePurchaseOrderReq {
    /// 供应商。
    pub supplier_id: String,
    /// 下单日期。
    pub order_date: NaiveDate,
    /// 期望到货日。
    #[serde(default)]
    pub expect_date: Option<NaiveDate>,
    /// 收货仓库。
    pub warehouse_id: String,
    /// 币种（缺省 `CNY`，`CHAR(3)`）。
    #[serde(default = "default_currency")]
    pub currency_code: String,
    /// 账期天数（`>= 0`，缺省 0）。
    #[serde(default)]
    pub credit_days: i32,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

impl CreatePurchaseOrderReq {
    /// 校验（`400 + 10001`）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        require_text(&self.supplier_id, "supplier_id", 36)?;
        require_text(&self.warehouse_id, "warehouse_id", 36)?;
        require_text(&self.currency_code, "currency_code", 3)?;
        if let Some(expect_date) = self.expect_date
            && expect_date < self.order_date
        {
            return Err(bad_request(format!(
                "expect_date（{expect_date}）不能早于 order_date（{}）",
                self.order_date
            )));
        }
        if self.credit_days < 0 {
            return Err(bad_request(format!(
                "credit_days 不能为负数（当前 {}）",
                self.credit_days
            )));
        }
        check_optional_text(self.remark.as_deref(), "remark", 2000)
    }
}

/// 编辑采购订单（只允许改「未提交」的单头信息；状态流转走 `PATCH /status`，W1c）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdatePurchaseOrderReq {
    /// 期望到货日（`Some(None)` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub expect_date: Option<Option<NaiveDate>>,
    /// 收货仓库。
    #[serde(default)]
    pub warehouse_id: Option<String>,
    /// 账期天数。
    #[serde(default)]
    pub credit_days: Option<i32>,
    /// 备注。
    #[serde(default, deserialize_with = "double_option")]
    pub remark: Option<Option<String>>,
}

impl UpdatePurchaseOrderReq {
    /// 校验。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(credit_days) = self.credit_days
            && credit_days < 0
        {
            return Err(bad_request(format!(
                "credit_days 不能为负数（当前 {credit_days}）"
            )));
        }
        Ok(())
    }
}

/// 采购订单筛选（分页）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PurchaseOrderFilter {
    /// 关键词（单号 / 备注）。
    pub keyword: Option<String>,
    /// 状态。
    pub status: Option<String>,
    /// 供应商。
    pub supplier_id: Option<String>,
    /// 收货仓库。
    pub warehouse_id: Option<String>,
    /// 下单日期起（含）。
    pub date_from: Option<NaiveDate>,
    /// 下单日期止（含）。
    pub date_to: Option<NaiveDate>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl Default for PurchaseOrderFilter {
    fn default() -> Self {
        Self {
            keyword: None,
            status: None,
            supplier_id: None,
            warehouse_id: None,
            date_from: None,
            date_to: None,
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

impl PurchaseOrderFilter {
    /// 校验（状态白名单 + 日期区间）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(status) = self.status.as_deref() {
            require_one_of(status, &PURCHASE_ORDER_STATUSES, "status")?;
        }
        if let (Some(from), Some(to)) = (self.date_from, self.date_to)
            && from > to
        {
            return Err(bad_request(format!(
                "date_from（{from}）不能晚于 date_to（{to}）"
            )));
        }
        Ok(())
    }
}

/// 采购订单列表卡片。
#[derive(Debug, Clone, Serialize)]
pub struct PurchaseOrderCard {
    /// 主键。
    pub id: String,
    /// 单号。
    pub order_no: String,
    /// 供应商。
    pub supplier_id: String,
    /// 下单日期。
    pub order_date: NaiveDate,
    /// 期望到货日。
    pub expect_date: Option<NaiveDate>,
    /// 收货仓库。
    pub warehouse_id: String,
    /// 状态。
    pub status: String,
    /// 合计数量。
    pub total_qty: Decimal,
    /// 合计含税金额（列表按含税展示）。
    pub total_with_tax: Decimal,
    /// 币种。
    pub currency_code: String,
}

impl From<&PurchaseOrderRow> for PurchaseOrderCard {
    fn from(row: &PurchaseOrderRow) -> Self {
        Self {
            id: row.id.to_string(),
            order_no: row.order_no.clone(),
            supplier_id: row.supplier_id.to_string(),
            order_date: row.order_date,
            expect_date: row.expect_date,
            warehouse_id: row.warehouse_id.to_string(),
            status: row.status.clone(),
            total_qty: row.total_qty,
            total_with_tax: row.total_with_tax,
            currency_code: row.currency_code.clone(),
        }
    }
}

/// 采购订单详情（含金额三段：不含税 / 税 / 含税）。
#[derive(Debug, Clone, Serialize)]
pub struct PurchaseOrderRes {
    /// 主键。
    pub id: String,
    /// 单号。
    pub order_no: String,
    /// 供应商。
    pub supplier_id: String,
    /// 下单日期。
    pub order_date: NaiveDate,
    /// 期望到货日。
    pub expect_date: Option<NaiveDate>,
    /// 收货仓库。
    pub warehouse_id: String,
    /// 状态。
    pub status: String,
    /// 币种。
    pub currency_code: String,
    /// 合计数量。
    pub total_qty: Decimal,
    /// 不含税金额。
    pub total_amount: Decimal,
    /// 税额。
    pub total_tax: Decimal,
    /// 含税金额。
    pub total_with_tax: Decimal,
    /// 账期天数。
    pub credit_days: i32,
    /// 备注。
    pub remark: Option<String>,
    /// 金额口径（`EXCL` 不含税录入 / `INCL` 含税录入）。
    pub price_mode: String,
    /// 提交时间。
    pub submitted_at: Option<DateTime<Utc>>,
    /// 提交人。
    pub submitted_by: Option<String>,
    /// 审批时间。
    pub approved_at: Option<DateTime<Utc>>,
    /// 审批人。
    pub approved_by: Option<String>,
    /// 关闭时间。
    pub closed_at: Option<DateTime<Utc>>,
    /// 关闭人。
    pub closed_by: Option<String>,
    /// 审批实例 id（P-FLOW-3A；`None` = 没走审批 / 已驳回打回）。前端用它跳到审批单详情看进度。
    pub approval_instance_id: Option<String>,
    /// 审批实例单号（`FLW-…`，与 `approval_instance_id` 同源；列表直接展示「审批中（FLW-xxx）」）。
    ///
    /// 由 handler 回填（DTO 的 `From<&Row>` 只拿到单据行，查不到实例单号）。
    pub approval_instance_no: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl From<&PurchaseOrderRow> for PurchaseOrderRes {
    fn from(row: &PurchaseOrderRow) -> Self {
        Self {
            id: row.id.to_string(),
            order_no: row.order_no.clone(),
            supplier_id: row.supplier_id.to_string(),
            order_date: row.order_date,
            expect_date: row.expect_date,
            warehouse_id: row.warehouse_id.to_string(),
            status: row.status.clone(),
            currency_code: row.currency_code.clone(),
            total_qty: row.total_qty,
            total_amount: row.total_amount,
            total_tax: row.total_tax,
            total_with_tax: row.total_with_tax,
            credit_days: row.credit_days,
            remark: row.remark.clone(),
            price_mode: row.price_mode.clone(),
            submitted_at: row.submitted_at,
            submitted_by: row.submitted_by.map(|id| id.to_string()),
            approved_at: row.approved_at,
            approved_by: row.approved_by.map(|id| id.to_string()),
            closed_at: row.closed_at,
            closed_by: row.closed_by.map(|id| id.to_string()),
            approval_instance_id: row.approval_instance_id.map(|id| id.to_string()),
            approval_instance_no: None,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

fn default_currency() -> String {
    "CNY".to_string()
}

// ==================== ⑩ 采购明细 ====================

/// 新建采购明细行。
///
/// 金额三件套必须自洽（DB `ck_biz_po_item_tax`：`with_tax_amount = amount + tax_amount`），
/// 这里在 `validate` 里先算一遍，别等数据库拒绝——错误文案要能指出是哪一行。
#[derive(Debug, Clone, Deserialize)]
pub struct CreatePurchaseOrderItemReq {
    /// 所属订单。
    pub order_id: String,
    /// 行号（订单内唯一）。
    pub line_no: i32,
    /// 物料。
    pub material_id: String,
    /// 计量单位。
    pub unit_id: String,
    /// 数量（`> 0`）。
    pub qty: Decimal,
    /// 单价（`>= 0`）。
    pub price: Decimal,
    /// 税率（`0`–`1`，缺省 0）。
    #[serde(default)]
    pub tax_rate: Option<Decimal>,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

impl CreatePurchaseOrderItemReq {
    /// 不含税金额（`qty × price`，落库前按 4 位小数取整，与 `NUMERIC(18,4)` 同标度）。
    pub fn amount(&self) -> Decimal {
        (self.qty * self.price).round_dp(4)
    }

    /// 税额（`amount × tax_rate`）。
    pub fn tax_amount(&self) -> Decimal {
        let rate = self.tax_rate.unwrap_or(Decimal::ZERO);
        (self.amount() * rate).round_dp(4)
    }

    /// 含税金额（`amount + tax_amount`，与被 DB 约束校验的值同源）。
    pub fn with_tax_amount(&self) -> Decimal {
        self.amount() + self.tax_amount()
    }

    /// 校验（`400 + 10001`）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        require_text(&self.order_id, "order_id", 36)?;
        require_text(&self.material_id, "material_id", 36)?;
        require_text(&self.unit_id, "unit_id", 36)?;
        if self.line_no < 1 {
            return Err(bad_request(format!(
                "line_no 必须 >= 1（当前 {}）",
                self.line_no
            )));
        }
        require_positive(self.qty, "qty")?;
        require_non_negative(self.price, "price")?;
        if let Some(tax_rate) = self.tax_rate {
            require_tax_rate(tax_rate)?;
        }
        check_optional_text(self.remark.as_deref(), "remark", 255)
    }
}

/// 编辑采购明细行（数量 / 单价 / 税率 / 备注；`line_no` 与物料不可改）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdatePurchaseOrderItemReq {
    /// 数量。
    #[serde(default)]
    pub qty: Option<Decimal>,
    /// 单价。
    #[serde(default)]
    pub price: Option<Decimal>,
    /// 税率。
    #[serde(default)]
    pub tax_rate: Option<Decimal>,
    /// 备注。
    #[serde(default, deserialize_with = "double_option")]
    pub remark: Option<Option<String>>,
}

impl UpdatePurchaseOrderItemReq {
    /// 校验。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(qty) = self.qty {
            require_positive(qty, "qty")?;
        }
        if let Some(price) = self.price {
            require_non_negative(price, "price")?;
        }
        if let Some(tax_rate) = self.tax_rate {
            require_tax_rate(tax_rate)?;
        }
        Ok(())
    }
}

/// 采购明细筛选（分页）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PurchaseOrderItemFilter {
    /// 所属订单。
    pub order_id: Option<String>,
    /// 物料。
    pub material_id: Option<String>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl Default for PurchaseOrderItemFilter {
    fn default() -> Self {
        Self {
            order_id: None,
            material_id: None,
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

/// 采购明细卡片（列表用；详情与卡片同构，见 [`PurchaseOrderItemRes`]）。
#[derive(Debug, Clone, Serialize)]
pub struct PurchaseOrderItemCard {
    /// 主键。
    pub id: String,
    /// 所属订单。
    pub order_id: String,
    /// 行号。
    pub line_no: i32,
    /// 物料。
    pub material_id: String,
    /// 数量。
    pub qty: Decimal,
    /// 单价。
    pub price: Decimal,
    /// 含税金额。
    pub with_tax_amount: Decimal,
    /// 已收货数量。
    pub received_qty: Decimal,
}

impl From<&PurchaseOrderItemRow> for PurchaseOrderItemCard {
    fn from(row: &PurchaseOrderItemRow) -> Self {
        Self {
            id: row.id.to_string(),
            order_id: row.order_id.to_string(),
            line_no: row.line_no,
            material_id: row.material_id.to_string(),
            qty: row.qty,
            price: row.price,
            with_tax_amount: row.with_tax_amount,
            received_qty: row.received_qty,
        }
    }
}

/// 采购明细详情。
#[derive(Debug, Clone, Serialize)]
pub struct PurchaseOrderItemRes {
    /// 主键。
    pub id: String,
    /// 所属订单。
    pub order_id: String,
    /// 行号。
    pub line_no: i32,
    /// 物料。
    pub material_id: String,
    /// 计量单位。
    pub unit_id: String,
    /// 数量。
    pub qty: Decimal,
    /// 单价。
    pub price: Decimal,
    /// 税率。
    pub tax_rate: Decimal,
    /// 不含税金额。
    pub amount: Decimal,
    /// 税额。
    pub tax_amount: Decimal,
    /// 含税金额。
    pub with_tax_amount: Decimal,
    /// 已收货数量。
    pub received_qty: Decimal,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl From<&PurchaseOrderItemRow> for PurchaseOrderItemRes {
    fn from(row: &PurchaseOrderItemRow) -> Self {
        Self {
            id: row.id.to_string(),
            order_id: row.order_id.to_string(),
            line_no: row.line_no,
            material_id: row.material_id.to_string(),
            unit_id: row.unit_id.to_string(),
            qty: row.qty,
            price: row.price,
            tax_rate: row.tax_rate,
            amount: row.amount,
            tax_amount: row.tax_amount,
            with_tax_amount: row.with_tax_amount,
            received_qty: row.received_qty,
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

// ==================== ⑪ 即时库存（成本账） ====================

/// 新建/初始化库存行（`(物料, 仓库)` 唯一，重复插入应 409，由 W1c 判）。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateInventoryReq {
    /// 物料。
    pub material_id: String,
    /// 仓库。
    pub warehouse_id: String,
    /// 初始数量（`>= 0`）。
    #[serde(default)]
    pub qty: Option<Decimal>,
    /// 初始均价（`>= 0`）。
    #[serde(default)]
    pub avg_cost: Option<Decimal>,
    /// 初始金额（`>= 0`）。
    #[serde(default)]
    pub amount: Option<Decimal>,
}

impl CreateInventoryReq {
    /// 校验（`400 + 10001`）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        require_text(&self.material_id, "material_id", 36)?;
        require_text(&self.warehouse_id, "warehouse_id", 36)?;
        for (value, field) in [
            (self.qty, "qty"),
            (self.avg_cost, "avg_cost"),
            (self.amount, "amount"),
        ] {
            if let Some(value) = value {
                require_non_negative(value, field)?;
            }
        }
        Ok(())
    }
}

/// 调整库存（**只能改锁定量**：数量与金额必须由流水驱动，直接改会破坏账实一致）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateInventoryReq {
    /// 锁定量（`>= 0`）。
    #[serde(default)]
    pub locked_qty: Option<Decimal>,
}

impl UpdateInventoryReq {
    /// 校验。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(locked_qty) = self.locked_qty {
            require_non_negative(locked_qty, "locked_qty")?;
        }
        Ok(())
    }
}

/// 库存筛选（分页）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct InventoryFilter {
    /// 物料。
    pub material_id: Option<String>,
    /// 仓库。
    pub warehouse_id: Option<String>,
    /// 只看有货的（`qty <> 0`）。
    pub only_nonzero: bool,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl Default for InventoryFilter {
    fn default() -> Self {
        Self {
            material_id: None,
            warehouse_id: None,
            only_nonzero: false,
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

/// 库存列表卡片。
#[derive(Debug, Clone, Serialize)]
pub struct InventoryCard {
    /// 主键。
    pub id: String,
    /// 物料。
    pub material_id: String,
    /// 仓库。
    pub warehouse_id: String,
    /// 账面数量。
    pub qty: Decimal,
    /// 锁定量。
    pub locked_qty: Decimal,
    /// 均价。
    pub avg_cost: Decimal,
    /// 账面金额。
    pub amount: Decimal,
    /// 最近流水时间。
    pub last_txn_at: Option<DateTime<Utc>>,
}

impl From<&InventoryRow> for InventoryCard {
    fn from(row: &InventoryRow) -> Self {
        Self {
            id: row.id.to_string(),
            material_id: row.material_id.to_string(),
            warehouse_id: row.warehouse_id.to_string(),
            qty: row.qty,
            locked_qty: row.locked_qty,
            avg_cost: row.avg_cost,
            amount: row.amount,
            last_txn_at: row.last_txn_at,
        }
    }
}

/// 库存详情（与卡片同构，多时间戳）。
#[derive(Debug, Clone, Serialize)]
pub struct InventoryRes {
    /// 主键。
    pub id: String,
    /// 物料。
    pub material_id: String,
    /// 仓库。
    pub warehouse_id: String,
    /// 账面数量。
    pub qty: Decimal,
    /// 锁定量。
    pub locked_qty: Decimal,
    /// 均价。
    pub avg_cost: Decimal,
    /// 账面金额。
    pub amount: Decimal,
    /// 最近流水时间。
    pub last_txn_at: Option<DateTime<Utc>>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl From<&InventoryRow> for InventoryRes {
    fn from(row: &InventoryRow) -> Self {
        Self {
            id: row.id.to_string(),
            material_id: row.material_id.to_string(),
            warehouse_id: row.warehouse_id.to_string(),
            qty: row.qty,
            locked_qty: row.locked_qty,
            avg_cost: row.avg_cost,
            amount: row.amount,
            last_txn_at: row.last_txn_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

// ==================== ⑫ 库位库存（数量账） ====================

/// 新建库位库存行（`(物料, 仓库, 库位)` 唯一，`location_id` 可空）。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateInventoryLocationReq {
    /// 物料。
    pub material_id: String,
    /// 仓库。
    pub warehouse_id: String,
    /// 库位（可空 = 未分配库位的数量）。
    #[serde(default)]
    pub location_id: Option<String>,
    /// 数量（`>= 0`）。
    #[serde(default)]
    pub qty: Option<Decimal>,
}

impl CreateInventoryLocationReq {
    /// 校验（`400 + 10001`）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        require_text(&self.material_id, "material_id", 36)?;
        require_text(&self.warehouse_id, "warehouse_id", 36)?;
        if let Some(qty) = self.qty {
            require_non_negative(qty, "qty")?;
        }
        Ok(())
    }
}

/// 调整库位库存（同样只允许改锁定量）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateInventoryLocationReq {
    /// 锁定量（`>= 0`）。
    #[serde(default)]
    pub locked_qty: Option<Decimal>,
}

impl UpdateInventoryLocationReq {
    /// 校验。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(locked_qty) = self.locked_qty {
            require_non_negative(locked_qty, "locked_qty")?;
        }
        Ok(())
    }
}

/// 库位库存筛选（分页）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct InventoryLocationFilter {
    /// 物料。
    pub material_id: Option<String>,
    /// 仓库。
    pub warehouse_id: Option<String>,
    /// 库位。
    pub location_id: Option<String>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl Default for InventoryLocationFilter {
    fn default() -> Self {
        Self {
            material_id: None,
            warehouse_id: None,
            location_id: None,
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

/// 库位库存卡片。
#[derive(Debug, Clone, Serialize)]
pub struct InventoryLocationCard {
    /// 主键。
    pub id: String,
    /// 物料。
    pub material_id: String,
    /// 仓库。
    pub warehouse_id: String,
    /// 库位。
    pub location_id: Option<String>,
    /// 数量。
    pub qty: Decimal,
    /// 锁定量。
    pub locked_qty: Decimal,
}

impl From<&InventoryLocationRow> for InventoryLocationCard {
    fn from(row: &InventoryLocationRow) -> Self {
        Self {
            id: row.id.to_string(),
            material_id: row.material_id.to_string(),
            warehouse_id: row.warehouse_id.to_string(),
            location_id: row.location_id.map(|id| id.to_string()),
            qty: row.qty,
            locked_qty: row.locked_qty,
        }
    }
}

/// 库位库存详情。
#[derive(Debug, Clone, Serialize)]
pub struct InventoryLocationRes {
    /// 主键。
    pub id: String,
    /// 物料。
    pub material_id: String,
    /// 仓库。
    pub warehouse_id: String,
    /// 库位。
    pub location_id: Option<String>,
    /// 数量。
    pub qty: Decimal,
    /// 锁定量。
    pub locked_qty: Decimal,
    /// 最近流水时间。
    pub last_txn_at: Option<DateTime<Utc>>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl From<&InventoryLocationRow> for InventoryLocationRes {
    fn from(row: &InventoryLocationRow) -> Self {
        Self {
            id: row.id.to_string(),
            material_id: row.material_id.to_string(),
            warehouse_id: row.warehouse_id.to_string(),
            location_id: row.location_id.map(|id| id.to_string()),
            qty: row.qty,
            locked_qty: row.locked_qty,
            last_txn_at: row.last_txn_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

// ==================== ⑬ 库存流水 ====================

/// 记账流水（**只增不改**：没有 `UpdateInventoryTxnReq`）。
///
/// `qty` 恒为正数，方向由 `txn_type`（`IN` / `OUT`）表达（DB `ck_biz_inv_txn_qty: qty > 0`）；
/// `before_* / after_*` 六列由 W1c 在事务内按当前库存算好再写（不写 `now()`，见 skill 教训）。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateInventoryTxnReq {
    /// 物料。
    pub material_id: String,
    /// 仓库。
    pub warehouse_id: String,
    /// 出入方向（`IN` / `OUT`）。
    pub txn_type: String,
    /// 业务类型（如 `PURCHASE`）。
    pub biz_type: String,
    /// 单据类型（如 `PURCHASE_ORDER`）。
    pub doc_type: String,
    /// 单据主键。
    pub doc_id: String,
    /// 单据号。
    pub doc_no: String,
    /// 单据行号（缺省 1）。
    #[serde(default = "default_step")]
    pub line_no: i32,
    /// 数量（`> 0`）。
    pub qty: Decimal,
    /// 单位成本（`>= 0`）。
    #[serde(default)]
    pub unit_cost: Option<Decimal>,
    /// 金额（`>= 0`）。
    #[serde(default)]
    pub amount: Option<Decimal>,
    /// 会计期间编码（`YYYY-MM`）。
    pub period_code: String,
    /// 操作人。
    #[serde(default)]
    pub operator_id: Option<String>,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

impl CreateInventoryTxnReq {
    /// 校验（`400 + 10001`）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        require_text(&self.material_id, "material_id", 36)?;
        require_text(&self.warehouse_id, "warehouse_id", 36)?;
        require_one_of(&self.txn_type, &TXN_TYPES, "txn_type")?;
        require_text(&self.biz_type, "biz_type", 24)?;
        require_text(&self.doc_type, "doc_type", 24)?;
        require_text(&self.doc_id, "doc_id", 36)?;
        require_text(&self.doc_no, "doc_no", 32)?;
        require_text(&self.period_code, "period_code", 7)?;
        require_positive(self.qty, "qty")?;
        if self.line_no < 1 {
            return Err(bad_request(format!(
                "line_no 必须 >= 1（当前 {}）",
                self.line_no
            )));
        }
        for (value, field) in [(self.unit_cost, "unit_cost"), (self.amount, "amount")] {
            if let Some(value) = value {
                require_non_negative(value, field)?;
            }
        }
        check_optional_text(self.remark.as_deref(), "remark", 255)
    }
}

/// 流水筛选（分页）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct InventoryTxnFilter {
    /// 物料。
    pub material_id: Option<String>,
    /// 仓库。
    pub warehouse_id: Option<String>,
    /// 单据号。
    pub doc_no: Option<String>,
    /// 单据类型。
    pub doc_type: Option<String>,
    /// 出入方向。
    pub txn_type: Option<String>,
    /// 会计期间编码。
    pub period_code: Option<String>,
    /// 记账号区间起始日（含）。
    pub date_from: Option<NaiveDate>,
    /// 记账号区间结束日（含）。
    pub date_to: Option<NaiveDate>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl Default for InventoryTxnFilter {
    fn default() -> Self {
        Self {
            material_id: None,
            warehouse_id: None,
            doc_no: None,
            doc_type: None,
            txn_type: None,
            period_code: None,
            date_from: None,
            date_to: None,
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

impl InventoryTxnFilter {
    /// 校验。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(txn_type) = self.txn_type.as_deref() {
            require_one_of(txn_type, &TXN_TYPES, "txn_type")?;
        }
        if let (Some(from), Some(to)) = (self.date_from, self.date_to)
            && from > to
        {
            return Err(bad_request(format!(
                "date_from（{from}）不能晚于 date_to（{to}）"
            )));
        }
        Ok(())
    }
}

/// 流水列表卡片。
#[derive(Debug, Clone, Serialize)]
pub struct InventoryTxnCard {
    /// 主键。
    pub id: String,
    /// 物料。
    pub material_id: String,
    /// 仓库。
    pub warehouse_id: String,
    /// 方向。
    pub txn_type: String,
    /// 单据号。
    pub doc_no: String,
    /// 数量。
    pub qty: Decimal,
    /// 金额。
    pub amount: Decimal,
    /// 变动后数量。
    pub after_qty: Decimal,
    /// 记账时间。
    pub created_at: DateTime<Utc>,
}

impl From<&InventoryTxnRow> for InventoryTxnCard {
    fn from(row: &InventoryTxnRow) -> Self {
        Self {
            id: row.id.to_string(),
            material_id: row.material_id.to_string(),
            warehouse_id: row.warehouse_id.to_string(),
            txn_type: row.txn_type.clone(),
            doc_no: row.doc_no.clone(),
            qty: row.qty,
            amount: row.amount,
            after_qty: row.after_qty,
            created_at: row.created_at,
        }
    }
}

/// 流水详情（前后六列全给，用于对账）。
#[derive(Debug, Clone, Serialize)]
pub struct InventoryTxnRes {
    /// 主键。
    pub id: String,
    /// 物料。
    pub material_id: String,
    /// 仓库。
    pub warehouse_id: String,
    /// 方向。
    pub txn_type: String,
    /// 业务类型。
    pub biz_type: String,
    /// 单据类型。
    pub doc_type: String,
    /// 单据主键。
    pub doc_id: String,
    /// 单据号。
    pub doc_no: String,
    /// 单据行号。
    pub line_no: i32,
    /// 数量。
    pub qty: Decimal,
    /// 单位成本。
    pub unit_cost: Decimal,
    /// 金额。
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
    pub operator_id: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 记账时间。
    pub created_at: DateTime<Utc>,
}

impl From<&InventoryTxnRow> for InventoryTxnRes {
    fn from(row: &InventoryTxnRow) -> Self {
        Self {
            id: row.id.to_string(),
            material_id: row.material_id.to_string(),
            warehouse_id: row.warehouse_id.to_string(),
            txn_type: row.txn_type.clone(),
            biz_type: row.biz_type.clone(),
            doc_type: row.doc_type.clone(),
            doc_id: row.doc_id.to_string(),
            doc_no: row.doc_no.clone(),
            line_no: row.line_no,
            qty: row.qty,
            unit_cost: row.unit_cost,
            amount: row.amount,
            before_qty: row.before_qty,
            before_amount: row.before_amount,
            before_cost: row.before_cost,
            after_qty: row.after_qty,
            after_amount: row.after_amount,
            after_cost: row.after_cost,
            period_code: row.period_code.clone(),
            operator_id: row.operator_id.map(|id| id.to_string()),
            remark: row.remark.clone(),
            created_at: row.created_at,
        }
    }
}

// ==================== 单元测试（纯逻辑，无需数据库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::uuid::Uuid;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("测试金额字面量非法")
    }

    fn unit_req() -> CreateUnitReq {
        CreateUnitReq {
            unit_code: "PCS".to_string(),
            unit_name: "个".to_string(),
            is_base: true,
            base_unit_id: None,
            rate: Some(Decimal::ONE),
            status: 1,
            sort_order: 0,
            remark: None,
        }
    }

    /// Acceptance ⑤-1：空编码被拒。
    #[test]
    fn validate_rejects_blank_code() {
        let mut req = unit_req();
        req.unit_code = "   ".to_string();
        let err = req.validate().expect_err("空编码必须被拒");
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(err.msg.contains("unit_code"), "文案要点名字段：{}", err.msg);

        let mut material = material_req();
        material.material_code = String::new();
        assert!(material.validate().is_err(), "物料空编码同样被拒");
    }

    /// Acceptance ⑤-2：超长名称被拒（与 `VARCHAR(50)` 同口径）。
    #[test]
    fn validate_rejects_overlong_name() {
        let mut req = unit_req();
        req.unit_name = "个".repeat(51);
        let err = req.validate().expect_err("超长名称必须被拒");
        assert!(
            err.msg.contains("unit_name") && err.msg.contains("50"),
            "{}",
            err.msg
        );

        // 边界：正好 50 个字符要放过（不多拒）
        let mut ok = unit_req();
        ok.unit_name = "个".repeat(50);
        assert!(ok.validate().is_ok(), "50 个字符是合法上界");

        // 中文按字符数算，不按字节数（"个" 是 3 字节，按字节会在 17 个字符时误拒）
        let mut chinese = unit_req();
        chinese.unit_name = "计量单位".repeat(12); // 48 字符 / 144 字节
        assert!(chinese.validate().is_ok(), "长度按 char 计，不按 byte");
    }

    /// Acceptance ⑤-3：负数量被拒。
    #[test]
    fn validate_rejects_negative_qty() {
        let mut item = po_item_req();
        item.qty = dec("-1");
        let err = item.validate().expect_err("负数量必须被拒");
        assert!(err.msg.contains("qty"), "{}", err.msg);

        let mut zero = po_item_req();
        zero.qty = Decimal::ZERO;
        assert!(zero.validate().is_err(), "0 数量同样不合法（DB: qty > 0）");

        let mut inventory = CreateInventoryReq {
            material_id: Uuid::now_v7().to_string(),
            warehouse_id: Uuid::now_v7().to_string(),
            qty: Some(dec("-0.0001")),
            avg_cost: None,
            amount: None,
        };
        assert!(inventory.validate().is_err(), "库存负数被拒");
        inventory.qty = Some(Decimal::ZERO);
        assert!(inventory.validate().is_ok(), "库存 0 是合法的（空仓行）");
    }

    /// 单位：基础单位带折算基准 / 折算单位缺 rate，两头都拒。
    #[test]
    fn validate_unit_base_consistency() {
        let mut req = unit_req();
        req.base_unit_id = Some(Uuid::now_v7().to_string());
        assert!(req.validate().is_err(), "基础单位不能带 base_unit_id");

        let mut derived = unit_req();
        derived.is_base = false;
        derived.base_unit_id = Some(Uuid::now_v7().to_string());
        derived.rate = None;
        assert!(derived.validate().is_err(), "折算单位必须给 rate");

        derived.rate = Some(dec("12"));
        assert!(derived.validate().is_ok(), "折算单位 12 个/基础单位合法");
    }

    /// 仓库：顶级 vs 下级仓库的 `level` 必须自洽，且上限 3。
    #[test]
    fn validate_warehouse_level_consistency() {
        let mut req = warehouse_req();
        req.level = 2;
        assert!(req.validate().is_err(), "顶级仓库 level=2 不合法");
        req.level = 1;
        assert!(req.validate().is_ok());

        let mut child = warehouse_req();
        child.parent_id = Some(Uuid::now_v7().to_string());
        child.level = 3;
        assert!(child.validate().is_ok(), "库房（3 级）合法");
        child.level = 4;
        assert!(child.validate().is_err(), "超过 3 级被拒");
    }

    /// 枚举白名单：物料类型 / 计价方式 / 期间状态。
    #[test]
    fn validate_rejects_unknown_enum_values() {
        let mut material = material_req();
        material.material_type = "PACKAGING".to_string();
        let err = material.validate().expect_err("未知物料类型必须被拒");
        assert!(err.msg.contains("material_type"), "{}", err.msg);

        material.material_type = "RAW".to_string();
        material.costing_method = "LIFO".to_string();
        assert!(material.validate().is_err(), "未知计价方式被拒");

        material.costing_method = "FIFO".to_string();
        material.tax_rate = Some(dec("1.13"));
        assert!(material.validate().is_err(), "税率 > 1 被拒");
        material.tax_rate = Some(dec("0.13"));
        assert!(material.validate().is_ok());
    }

    /// 期间：月份越界 / 起止倒置 / 期间编码推导。
    #[test]
    fn validate_period_bounds_and_code() {
        let mut req = CreatePeriodReq {
            period_year: 2026,
            period_month: 10,
            start_date: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
            status: "OPEN".to_string(),
            remark: None,
        };
        assert_eq!(req.period_code(), "2026-10");
        assert!(req.validate().is_ok());

        req.period_month = 13;
        assert!(req.validate().is_err(), "月份 13 被拒");

        req.period_month = 10;
        req.end_date = NaiveDate::from_ymd_opt(2026, 9, 30).unwrap();
        assert!(req.validate().is_err(), "起止倒置被拒");
    }

    /// 采购明细：金额三件套自洽（与 DB 的 `ck_biz_po_item_tax` 同源）。
    #[test]
    fn purchase_order_item_amounts_are_consistent() {
        let item = po_item_req();
        assert_eq!(item.amount(), dec("100"));
        assert_eq!(item.tax_amount(), dec("13"));
        assert_eq!(item.with_tax_amount(), dec("113"));
        assert_eq!(
            item.with_tax_amount(),
            item.amount() + item.tax_amount(),
            "DB 约束要求含税 = 不含税 + 税"
        );

        // 4 位小数：0.3333 × 3 = 0.9999；单价 6 位小数的乘法结果按 4 位取整
        let mut odd = po_item_req();
        odd.qty = dec("3");
        odd.price = dec("0.333333");
        assert_eq!(odd.amount(), dec("1.0000"), "amount 按 4 位小数取整");
        assert_eq!(odd.with_tax_amount(), odd.amount() + odd.tax_amount());
    }

    /// Decimal 的 JSON 形态：序列化用字符串（不经过 f64），反序列化两种都收。
    #[test]
    fn decimal_json_roundtrip_keeps_scale() {
        let card = SupplierCard {
            id: Uuid::now_v7().to_string(),
            supplier_code: "S1".to_string(),
            supplier_name: "供应商".to_string(),
            short_name: None,
            contact_person: None,
            contact_phone: None,
            credit_days: 30,
            credit_limit: dec("12345.6789"),
            status: 1,
        };
        let json = serde_json::to_value(&card).expect("序列化");
        assert_eq!(
            json["credit_limit"],
            serde_json::Value::String("12345.6789".to_string()),
            "Decimal 默认以字符串序列化（JSON number 会过 f64，精度不可控）"
        );
        let back: Decimal = serde_json::from_value(json["credit_limit"].clone()).expect("反序列化");
        assert_eq!(back, dec("12345.6789"), "字符串往返不丢标度");
        // 数字字面量同样能反序列化（前端不一定按字符串传）
        let from_number: Decimal = serde_json::from_str("12345.6789").expect("数字反序列化");
        assert_eq!(from_number, dec("12345.6789"));
    }

    /// 编辑语义：字段缺席 ≠ 清空（`Option<Option<T>>`）。
    #[test]
    fn update_absent_means_unchanged() {
        // 缺席：remark 保持 None（不改）
        let absent: UpdateMaterialReq =
            serde_json::from_str(r#"{"material_name":"新名字"}"#).unwrap();
        assert_eq!(absent.remark, None);
        assert_eq!(absent.spec, None);
        // 显式 null：Some(None)（清空）
        let cleared: UpdateMaterialReq =
            serde_json::from_str(r#"{"remark":null,"spec":null}"#).unwrap();
        assert_eq!(cleared.remark, Some(None));
        assert_eq!(cleared.spec, Some(None));
        // 有值：Some(Some(v))
        let updated: UpdateMaterialReq =
            serde_json::from_str(r#"{"remark":"有值","spec":"M8"}"#).unwrap();
        assert_eq!(updated.remark, Some(Some("有值".to_string())));
        assert_eq!(updated.spec, Some(Some("M8".to_string())));
    }

    /// 筛选缺省值与分页口径（`page = 1`，`page_size = 默认 20`）。
    #[test]
    fn filters_have_sane_defaults() {
        let filter: MaterialFilter = serde_json::from_str("{}").unwrap();
        assert_eq!(filter.page, 1);
        assert_eq!(filter.page_size, DEFAULT_PAGE_SIZE);
        assert!(filter.keyword.is_none());
        assert!(filter.serial_required.is_none());
        let parsed: MaterialFilter =
            serde_json::from_str(r#"{"keyword":"螺栓","serial_required":true,"page":2}"#).unwrap();
        assert_eq!(parsed.page, 2);
        assert_eq!(parsed.serial_required, Some(true), "一物一码筛选能反序列化");
        assert_eq!(parsed.page_size, DEFAULT_PAGE_SIZE, "缺省每页条数");
    }

    /// 流水：方向白名单 + 数量必须为正。
    #[test]
    fn validate_txn_direction_and_qty() {
        let mut txn = CreateInventoryTxnReq {
            material_id: Uuid::now_v7().to_string(),
            warehouse_id: Uuid::now_v7().to_string(),
            txn_type: "IN".to_string(),
            biz_type: "PURCHASE".to_string(),
            doc_type: "PURCHASE_ORDER".to_string(),
            doc_id: Uuid::now_v7().to_string(),
            doc_no: "PO-1".to_string(),
            line_no: 1,
            qty: dec("5"),
            unit_cost: Some(dec("20")),
            amount: Some(dec("100")),
            period_code: "2026-10".to_string(),
            operator_id: None,
            remark: None,
        };
        assert!(txn.validate().is_ok());
        txn.txn_type = "ADJUST".to_string();
        assert!(txn.validate().is_err(), "未知方向被拒");
        txn.txn_type = "OUT".to_string();
        txn.qty = dec("-5");
        assert!(
            txn.validate().is_err(),
            "流水数量必须为正（方向由 txn_type 表达）"
        );
    }

    // ---------- 夹具 ----------

    fn material_req() -> CreateMaterialReq {
        CreateMaterialReq {
            material_code: "M-001".to_string(),
            material_name: "螺栓".to_string(),
            spec: Some("M8x20".to_string()),
            unit_id: Uuid::now_v7().to_string(),
            material_type: "RAW".to_string(),
            default_warehouse_id: None,
            safety_stock: Some(dec("10.5")),
            costing_method: "MOVING_AVG".to_string(),
            inventory_account_id: None,
            tax_rate: Some(dec("0.13")),
            status: 1,
            serial_required: None,
            remark: None,
        }
    }

    fn warehouse_req() -> CreateWarehouseReq {
        CreateWarehouseReq {
            warehouse_code: "WH-01".to_string(),
            warehouse_name: "总仓".to_string(),
            warehouse_type: "NORMAL".to_string(),
            manager: None,
            allow_negative: false,
            address: None,
            status: 1,
            sort_order: 0,
            remark: None,
            parent_id: None,
            level: 1,
        }
    }

    fn po_item_req() -> CreatePurchaseOrderItemReq {
        CreatePurchaseOrderItemReq {
            order_id: Uuid::now_v7().to_string(),
            line_no: 1,
            material_id: Uuid::now_v7().to_string(),
            unit_id: Uuid::now_v7().to_string(),
            qty: dec("5"),
            price: dec("20"),
            tax_rate: Some(dec("0.13")),
            remark: None,
        }
    }
}

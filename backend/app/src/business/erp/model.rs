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

// ==================== 部件（TD-1 拆分，facade 重导出） ====================
//
// 本文件只做 facade：常量 / 状态机 / 查询 / 写入 / 校验 分居各部件文件，
// 下面按顺序 `pub use` 拼回**与拆分前完全一致**的公开 API ——
// 所有 `use crate::business::erp::<本文件>::xxx` 的调用方一行不用改。

pub use super::model_base::*;
pub use super::model_inventory::*;
pub use super::model_master::*;
pub use super::model_order::*;

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;

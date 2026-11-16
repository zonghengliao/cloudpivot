//! 补货建议（M2-15）：安全库存 + 在途 + 未清订单 → **建议采购量**。
//!
//! 卡：t_527ceaa3（M2-15【M2·仓库】补货建议：安全库存 + 在途 + 未清订单）；
//! 拆卡依据 `docs/planning/20261113-新卡清单.md` §M2 的 M2-15 行。
//!
//! ```text
//! 表 / 读         biz_inventory（结存）× biz_material_stock_policy（水位，M2-13）
//!                 × biz_inventory_in_transit（发货在途，M2-01~M2-04）× biz_purchase_order(_item)（未清订单）
//! 阈值            stock_policy::dto::resolve_min_qty / resolve_max_qty —— 三级回退的唯一实现（不重写）
//! 判定            dto::suggest（纯函数）：预计可用 < 最低水位 → 补到目标水位（最高水位优先）
//! 接口            GET /business/erp/replenish/page   权限码 business:erp:replenish:list（V0430）
//! 写              零。建议量是**现算**的清单，不落表（落表就要在每次出入库 / 到货 / 改水位时维护）
//! ```
//!
//! # 这个模块与邻居的分工（别把别人的活干进来）
//!
//! | 模块 | 回答的问题 | 写不写库 |
//! |:--|:--|:--:|
//! | `stock_policy`（M2-13） | 一格的水位**设成多少** | 写（设置表） |
//! | `stock_policy::water_check`（M2-14） | 跌破水位**要不要发待办** | 写（`biz_task`） |
//! | `inventory_transit`（M2-04/05） | 在途**有多少、对不对** | 不写 |
//! | `replenish`（本模块，M2-15） | 该补多少、**建议采购多少** | 不写 |
//!
//! 本模块**不下采购单**（PRD E2-01 是采购模块自己的卡）、**不建待办**（M2-14 已有一条通道，
//! 两处都建会出现「同一格两个待办」）、**不改水位**（M2-13 的设置接口是唯一写入口）。
//!
//! # 容器文件
//!
//! * [`dto`] —— 入参 / 出参契约 + **判定口径的唯一实现** [`dto::suggest`] + 过滤 / 排序 / 汇总 /
//!   分页的纯函数；
//! * [`model`] —— 一条扫描 SQL（两个 `LATERAL` 预聚合，无笛卡尔积）+ 策略行三列 → `PolicyLevels`；
//! * [`handler`] —— 接口层：参数校验 + 编排（算 → 过滤 → 排序 → 汇总 → 分页）+ hints；
//! * 本文件 —— 模块地图与口径总述。
//!
//! 纯逻辑用例在 `dto` / `model` / `handler` 各自的 `mod tests`；真库用例见 [`replenish_db_tests`]
//! （`ERP_DB_URL` 开关，与其它 ERP 用例共用同一个串行锁）。

pub mod dto;
pub mod handler;
pub mod model;

/// 补货建议的真库用例（`ERP_DB_URL` 开关；串行跑）。
#[cfg(test)]
mod replenish_db_tests;

pub use dto::{
    CELL_SCAN_LIMIT, CellFacts, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, ReplenishFilter, ReplenishRes, ReplenishRowRes,
    ReplenishTotals, Suggestion, TARGET_MAX, TARGET_MIN, TARGET_NONE, VERDICT_COVERED, VERDICT_OK, VERDICT_PURCHASE,
    keep_row, suggest, verdict_label,
};
pub use handler::{PERM_LIST, REPLENISH_PERMISSIONS, replenish_page_of};
pub use model::{ReplenishCell, ScanFilters, scan_cells};

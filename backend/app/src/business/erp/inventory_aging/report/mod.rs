//! 库龄分段报表（M2-09）：按 **物料 × 仓库** 汇总 0-30 / 31-60 / 61-90 / 90 天以上 四档。
//!
//! 卡：t_4e2cb6f6（M2-09【M2·仓库】库龄分析：报表（0-30/31-60/61-90/90+））；
//! 拆卡依据 `docs/planning/20261113-新卡清单.md` §M2 的 M2-09 行。
//!
//! ```text
//! 流水                  biz_inventory_txn（V0137，追加写；IN 一笔 = 一个批次层）
//! 计算                  biz_inventory_aging_layers(p_as_of, p_material_id, p_warehouse_id)  ← V0424 · M2-08
//!                         └─ FIFO 摊层 + 业务日期快照：**唯一实现**，本模块只读它
//! 报表（本模块）         GROUP BY 物料, 仓库, 库龄档 → 出四档数量 / 参考金额 / 库龄统计
//! 接口                  GET /business/erp/inventory-aging/report
//! 权限码                business:erp:inventory-aging:report（V0425，menu_type=3）
//! 上游 / 下游           M2-08 计算 →（本卡）报表 → M2-10 呆滞料 / M2-12 前端页
//! ```
//!
//! # 为什么报表单独成一个子模块（不是把活干在 M2-08 的四个文件里）
//!
//! 「明细」与「报表」是**两张表、两个权限码、两种访问频度**：
//!
//! | | 明细（M2-08） | 报表（本卡） |
//! |:--|:--|:--|
//! | 粒度 | 一层入库一行（可按批号钻） | 物料 × 仓库 一行 |
//! | 分页 | 分页（`page` / `page_size`） | 不分页（行上限 2000 + 合计全量） |
//! | 权限码 | `…:inventory-aging:list` | `…:inventory-aging:report` |
//!
//! 混在一个文件里，两套契约（分页 vs 汇总 + 两套权限码）会互相污染出参；
//! 但**算法仍是同一份**：报表不认识 FIFO，只 `FROM` M2-08 的库函数。
//! 目录形态照 `payable/payable_payment/`、`hr/` 下的子模块四件套：
//! `mod.rs + dto.rs + model.rs + handler.rs`。
//!
//! # 四个容器文件
//!
//! * [`dto`] —— 档位表 [`dto::LAYER_BUCKETS`] + 入参 / 出参契约 + 纯逻辑
//!   （透视 `pivot_rows` / 合计 `summarise_rows` / 截断 `truncate_rows` / 分档 `bucket_of`）；
//! * [`model`] —— 一条聚合 SQL（`FROM` 库龄层函数，`GROUP BY 物料, 仓库, 档位`）；
//! * [`handler`] —— 接口层：参数校验 + 编排（**先合计再截断**）+ 批量补名 + hints + 错误收口；
//! * 本文件 —— 模块地图与口径总述。
//!
//! 纯逻辑用例在 [`dto`] / [`model`] / [`handler`] 各自的 `mod tests`；真库用例在
//! [`report_db_tests`]（`ERP_DB_URL` 开关，串行跑）。
//!
//! # 三条口径（写死，别在别处再解释）
//!
//! 1. **Σ各档 = Σ行 = Σ层剩余**：分档在 SQL 做、行合计 = Σ本行各档、全表合计 = Σ各行，
//!    只有一条加法路径（两条路径各算一次迟早会漂，且对不上时没人知道该信哪条）。
//! 2. **行上限只砍行**：合计与总量在截断前算完（截断了 `rows_truncated = true` +
//!    `hints` 明写，绝不静默少行）。
//! 3. **金额是参考金额**：层剩余 × 层入库单价，与移动加权账面成本本来不相等。

pub mod dto;
pub mod handler;
pub mod model;

/// 库龄分段报表的真库用例（`ERP_DB_URL` 开关；串行跑）。
#[cfg(test)]
mod report_db_tests;

pub use dto::{
    AgingBucketQtyRes, CellRow, InventoryAgingReportQuery, InventoryAgingReportRes, LAYER_BUCKETS,
    MaterialWarehouseAgingRes, REPORT_CODE, REPORT_NAME, ROW_LIMIT,
};
pub use handler::{INVENTORY_AGING_REPORT_PERMISSIONS, PERM_REPORT, report_of};
pub use model::{AGING_FUNCTION, cells, cells_params, cells_sql};

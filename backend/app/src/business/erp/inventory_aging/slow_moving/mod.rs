//! 呆滞料标记（M2-10）：库龄超过 N 天仍有剩余的 物料 × 仓库，N 可配置。
//!
//! 卡：t_bb0fe3c6（M2-10【M2·仓库】库龄：呆滞料标记（超 N 天无出库））；
//! 拆卡依据 `docs/planning/20261113-新卡清单.md` §M2 的 M2-10 行。
//!
//! ```text
//! 流水                  biz_inventory_txn（V0137，追加写；IN 一笔 = 一个批次层）
//! 计算                  biz_inventory_aging_layers(p_as_of, p_material_id, p_warehouse_id)  ← V0424 · M2-08
//!                         └─ FIFO 摊层 + 业务日期快照：**唯一实现**，本模块只读它
//! 标记（本模块）         GROUP BY 物料, 仓库：全部层 / 其中 is_slow = 库龄 > 阈值的层
//! 接口                  GET /business/erp/inventory-aging/slow-moving
//! 权限码                business:erp:inventory-aging:slow-moving（V0426，menu_type=3）
//! 阈值                  sys_config `erp.inventory.slow-moving-days`（V0426 种 90；参数 days 可临时覆盖）
//! 上游 / 下游           M2-08 计算 + M2-09 报表 →（本卡）呆滞标记 → M2-11 单测 / M2-12 前端页
//! ```
//!
//! # 为什么呆滞料单独成一个子模块
//!
//! 与 M2-09 报表单独成模块同一个理由：**三个视图、三个权限码、三种问题**。
//!
//! | | 明细（M2-08） | 报表（M2-09） | 呆滞料（本卡） |
//! |:--|:--|:--|:--|
//! | 粒度 | 一层入库一行 | 物料 × 仓库 × 档位 | 物料 × 仓库（只留呆滞的格） |
//! | 回答 | 哪一批压了多久 | 各档分布 | 超阈值那部分压了多少钱、占多少 |
//! | 阈值 | 无 | 固定 4 档 | **可配 N 天** |
//! | 权限码 | `…:list` | `…:report` | `…:slow-moving` |
//!
//! 但**算法仍只有一份**：三个视图都 `FROM biz_inventory_aging_layers(...)`，
//! 谁都没有第二份 FIFO（V0424 的模块头注释已把这条写死）。
//!
//! # 四个容器文件
//!
//! * [`dto`] —— 阈值（三级解析 / 校验 / 边界）、入参 / 出参契约、纯逻辑
//!   （`summarise_cells` / `build_rows` / `sort_rows` / `truncate_rows`）；
//! * [`model`] —— 一条聚合 SQL（库函数 + `is_slow` 布尔列 + 条件聚合）；
//! * [`handler`] —— 接口层：参数校验 + 阈值编排 + **先汇总再截断** + 批量补名 + hints；
//! * 本文件 —— 模块地图与口径总述。
//!
//! 纯逻辑用例在各文件的 `mod tests`；真库用例在 [`slow_moving_db_tests`]
//! （`ERP_DB_URL` 开关，与 M2-08 / M2-09 共用同一个串行锁）。

pub mod dto;
pub mod handler;
pub mod model;

/// 呆滞料标记的真库用例（`ERP_DB_URL` 开关；串行跑）。
#[cfg(test)]
mod slow_moving_db_tests;

pub use dto::{
    CellRow, DEFAULT_THRESHOLD_DAYS, MAX_THRESHOLD_DAYS, MIN_THRESHOLD_DAYS, ROW_LIMIT, SLOW_MOVING_CODE,
    SLOW_MOVING_NAME, SlowMovingQuery, SlowMovingRes, SlowMovingRowRes, SlowMovingSummaryRes, THRESHOLD_CONFIG_KEY,
    ThresholdResolution, ThresholdSource, is_slow, parse_threshold_param, resolve_threshold,
};
pub use handler::{INVENTORY_SLOW_MOVING_PERMISSIONS, PERM_SLOW_MOVING, slow_moving_of};
pub use model::{cells, cells_params, cells_sql};

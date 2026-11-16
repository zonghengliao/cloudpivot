//! 在途库存（M2-04 查询 / M2-05 对账）：`biz_inventory_in_transit` 的只读入口。
//!
//! 卡：t_f1bce846（M2-04【M2·仓库】在途库存：查询接口）、
//! t_b545b37a（M2-05【M2·仓库】在途库存：对账（在途 + 已出 = 发货总量））。
//! 拆卡依据 `docs/planning/20261113-新卡清单.md` §M2 的 M2-04 / M2-05 行；
//! 前端页面是 M2-07、专门的在途单测是 M2-06 —— 都不在本模块。
//!
//! ```text
//! 表 / 写入                 shipment::shipment_transit（V0420，M2-01 建表；
//!                           M2-02 写 shipped_qty、M2-03 推 received_qty）
//! 读（本模块）               inventory_transit::{dto, model, handler}       ← 明细 / 汇总（M2-04）
//!                           inventory_transit::reconcile                  ← 对账（M2-05）
//! 权限码                    business:erp:inventory-transit:list  （V0421，明细 / 汇总）
//!                           business:erp:inventory-transit:audit （V0422，对账）
//! ```
//!
//! # 为什么单独一个模块，而不是塞进 `shipment`
//!
//! ① 视角不同：`shipment` 是**单据域**（发货单 4 态状态机 + 过账 + 签收），本模块是
//!    **库存台账域**（按物料 / 仓库 / 客户查余量、对账），下游 M2-05（对账）与 M2-15
//!    （补货建议）读的是这一面；
//! ② 权限码不同：发货域是 `business:erp:shipment:*`，在途台账是
//!    `business:erp:inventory-transit:list` / `:audit`（与 `inventory:list` / `:audit`
//!    同形的台账读码 + 结论码）；
//! ③ 表名与状态域仍是**一份**：本模块不复制状态常量，一律
//!    `use shipment_transit::{TABLE, STATUS_*}`，避免「两个地方各写一遍 IN_TRANSIT」。
//!
//! # 四个容器文件
//!
//! * [`dto`] —— 筛选参数与出参契约（明细 / 汇总 / 对账）+ 纯逻辑校验（状态归一化 / 日期区间）；
//! * [`model`] —— 取数层：明细分页（走 `page_of`）、物料×仓库汇总、主数据批量补齐；
//! * [`reconcile`] —— 对账层（M2-05）：三侧比对（明细 / 已出 / 在途）、差异判定、汇总口径；
//! * [`handler`] —— 接口层：三条 `GET` + 名字批量补齐编排 + 错误收口。
//!
//! 真库用例见 `transit_query_tests`（M2-04）与 `transit_reconcile_tests`（M2-05），
//! 都只在 `cargo test` 时编译。

pub mod dto;
pub mod handler;
pub mod model;
pub mod reconcile;

/// 在途查询的真库用例（M2-04）。只在 `cargo test` 时编译。
#[cfg(test)]
mod transit_query_tests;

/// 在途对账的真库用例（M2-05）。只在 `cargo test` 时编译。
#[cfg(test)]
mod transit_reconcile_tests;

/// 在途「发货 → 签收」全流程单测（M2-06）：跨四张卡的一致性。只在 `cargo test` 时编译。
#[cfg(test)]
mod transit_flow_tests;

pub use dto::{
    TransitFilter, TransitReconcileFilter, TransitReconcileRes, TransitReconcileRow, TransitReconcileTotals,
    TransitRes, TransitSummaryFilter, TransitSummaryRes, TransitSummaryRow,
};
pub use handler::{INVENTORY_TRANSIT_PERMISSIONS, PERM_AUDIT, PERM_LIST};
pub use reconcile::{ComboRow, RECONCILE_SCAN_LIMIT, ReconcileQuery, ReconcileTotals, issue_label};

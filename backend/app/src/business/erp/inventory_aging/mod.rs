//! 库龄（M2-08）：**按批次入库时间算 FIFO 库龄层**的只读模块。
//!
//! 卡：t_397dc77c（M2-08【M2·仓库】库龄：计算（按批次入库时间））；拆卡依据
//! `docs/planning/20261113-新卡清单.md` §M2 的 M2-08 行 —— 本卡只做**计算**，
//! 0-30/31-60/61-90/90+ 的报表是 M2-09、呆滞料标记是 M2-10、前端页是 M2-12。
//!
//! ```text
//! 流水（唯一真相）        biz_inventory_txn（V0137，追加写；IN 一笔 = 一个批次层）
//! 计算（本卡）            biz_inventory_aging_layers(p_as_of, p_material_id, p_warehouse_id)
//!                           └─ V0424 建在库层的 SQL 函数：FIFO 累计 + 夹 0 + 业务日期
//! 取数（本卡）            inventory_aging::{dto, model, handler}   ← 批次层分页（GET …/page）
//! 互证（本卡）            inventory_aging::fifo                    ← 同一算法的 Rust 参考实现
//! 权限码                  business:erp:inventory-aging:list       （V0424，menu_type=3）
//! 下游                    M2-09 报表 / M2-10 呆滞料 / M2-12 前端页 共用同一个库函数
//! ```
//!
//! # 为什么把 FIFO 写进库函数，而不是写在每个调用方的 SQL 里
//!
//! 库龄 = 「结存按 FIFO 摊回各入库层」。这件事一旦在报表（M2-09）、呆滞料（M2-10）、
//! 明细（本卡）各写一遍 `SUM(...) OVER (ORDER BY ...)`，三处的排序键 / 日期口径 / 夹 0
//! 只要有一处漂，就会出现「明细说这层剩 120、报表说 110」而没有任何地方报错。
//! 所以：**算法在库层一份（V0424 的函数），Rust 侧只搬运**；Rust 里另有一份纯逻辑
//! 参考实现（[`fifo`]）专门用来和 SQL 对拍（真库用例 `sql_matches_reference_allocator`）。
//!
//! # 与 `stockout::stockout_model::allocate_fifo` 的区别（别混）
//!
//! 那个「FIFO」是**过账时从哪个库位桶扣货**（排序键 = 库位账行 `created_at`），
//! 管的是「货在哪个库位」；本模块的 FIFO 是**结存由哪些入库批次构成**（排序键 = 入库业务日期），
//! 管的是「这批货压了多久」。`stockout_model.rs` 里那句「真正的批次库龄 FIFO 要等批次卡」
//! 说的就是本模块 —— 表的形态没变（流水里只有物料 × 仓库），所以库龄层是**推导**出来的，
//! 不是另存一张层表（另存一份就要做出库时的层消耗记账，那是另一个量级的改造）。
//!
//! # 四个容器文件
//!
//! * [`dto`] —— 筛选与出参契约 + 纯逻辑（库龄天数 / 区间折算 / 批号归一化）；
//! * [`model`] —— 取数层：调库函数分页、主数据批量补名（一页 2 条 SQL，无 N+1）；
//! * [`handler`] —— 接口层：1 条 `GET` + 参数校验 + 名字补齐编排 + 错误收口；
//! * [`fifo`] —— FIFO 的 Rust 参考实现（只用于和 SQL 互证，生产路径不跑）。
//!
//! 真库用例见 [`aging_db_tests`]，纯逻辑用例见 [`aging_tests`] 与各文件的 `mod tests`，
//! 都只在 `cargo test` 时编译。

pub mod dto;
pub mod fifo;
pub mod handler;
pub mod model;

/// 库龄计算的纯逻辑用例（不连库）。
#[cfg(test)]
mod aging_tests;

/// 库龄计算的真库用例（`ERP_DB_URL` 开关；串行跑）。
#[cfg(test)]
mod aging_db_tests;

pub use dto::{AgingFilter, AgingLayerRes, age_days_of, inbound_bounds};
pub use fifo::{allocate_fifo, sum_of};
pub use handler::{INVENTORY_AGING_PERMISSIONS, PERM_LIST};
pub use model::{AGING_FUNCTION, LayerQuery, LayerRow, layer_page};

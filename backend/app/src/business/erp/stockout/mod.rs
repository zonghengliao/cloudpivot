//! 出库单域（原 `erp/stockout_*.rs`，P2-07 拆目录）：底座 / 数据层 / 契约 / 接口 / 联动接线。
//!
//! 只在本文件声明「原来在 `erp/mod.rs` 里声明」的 4 个模块；另外 4 个文件拆目录前就不是
//! `erp/mod.rs` 的成员，由 `#[path]` 就地挂载，**不要在此重复声明**：
//!
//! - `stockout_tests.rs` / `stockout_fifo_tests.rs` / `stockout_cost_object_tests.rs`
//!   → 由 `stockout_model` 用 `#[path]` 挂成子模块（`mod tests` / `mod fifo_tests` /
//!   `mod cost_object_tests`）；
//! - `stockout_linkage.rs` → 由 `stockout_handler` 用 `#[path]` 挂成 `mod linkage_hook`。
//!
//! `stockout_base` 拆前是 `erp` 下的私有 `mod`（= erp 子树可见），被 `erp/{purchase,sales}`、
//! `erp/{shipment_posting,receivable_bridge,stock_pdf}` 按绝对路径取，故写成
//! `pub(in crate::business::erp)` 保持逐字等价。

pub(in crate::business::erp) mod stockout_base; // 出库单底座：常量 / 状态机 / 行结构 / 取号
pub mod stockout_dto;
pub mod stockout_handler;
pub mod stockout_model;

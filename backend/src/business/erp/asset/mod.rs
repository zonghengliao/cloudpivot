//! 固定资产域（FIN-2 / LNK-09）：纯计算 / 契约 / 数据层 / 接口 / 处置回调 / 联动接线。
//!
//! P2-12 从 `erp/asset_*.rs` 纯移动而来（模块名不变，路径变 `erp::asset::asset_xxx`）。
//! `asset_model_tests.rs` 由 [`asset_model`] 的 `#[path = "asset_model_tests.rs"]` 挂载
//! （同目录、拆前拆后值不变），**不在本文件声明**——别当漏声明。

pub mod asset_bridge;
pub mod asset_compute;
pub mod asset_dto;
pub mod asset_handler;
pub mod asset_linkage;
pub mod asset_model;

/// LNK-09 的真库单测（固定资产联动接线：处置触发一次、重复处置不重复触发、
/// 作废处置触发、事务回滚通知不发、排除动作确实不跑）。只在 `cargo test` 时编译。
#[cfg(test)]
mod asset_linkage_tests;

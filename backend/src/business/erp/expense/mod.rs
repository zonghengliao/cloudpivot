//! 报销（费用）域：纯计算 / 契约 / 数据层 / 接口 / 审批回调 / 联动接线。
//!
//! P2-09 拆目录：本文件承接原 `erp/mod.rs` 里的 `expense_*` 声明块，
//! 模块名与可见性**逐字不变**（除 `expense_linkage` 为 `pub(crate)` 外均为 `pub mod`）。
//!
//! 本目录 8 个文件里 `expense_model_tests.rs` **不在此声明** —— 它由
//! `expense_model.rs` 用 `#[path = "expense_model_tests.rs"] mod …` 挂载，
//! 拆前拆后它与宿主同目录，`#[path]` 值一行未改。

pub mod expense_bridge;
pub mod expense_compute;
pub mod expense_dto;
pub mod expense_handler;
/// LNK-05 的**唯一**联动接线（提交 / 通过 / 付清 / 作废 / 打回五条路共用）。
/// `pub(crate)` 而不是私有：`expense_handler` 与 `expense_bridge` 两个宿主都要够得着，
/// 而 `expense_bridge` 又是被 `flow::handler` 调用的审批回调。
/// 联动上下文 / 收件人 / 幂等键只在那一份文件里维护（两处各写一份迟早漂移）。
pub(crate) mod expense_linkage;
pub mod expense_model;

/// LNK-05 的真库单测（报销单联动接线：提交 / 通过 / 付清 / 作废 / 打回触发，
/// 凭证不被重复生成，重复操作不重复触发）。只在 `cargo test` 时编译。
#[cfg(test)]
mod expense_linkage_tests;

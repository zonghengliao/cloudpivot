//! 预算域（`budget_*`）：预算单 / 12 月分解 / 占用记录 / 版本链 / 审批桥接。
//!
//! P2-11（t_2801f2f5）把 `erp/` 下的 8 个 `budget_*.rs` 整域搬进本目录（纯移动，零逻辑改动）。
//! 其中 3 个测试文件拆前就是 `#[path]` 挂在同域文件下（同目录 → `#[path]` 值一字未改），
//! **不进本文件**（别当漏声明）：
//! - `budget_dto_tests.rs`   ← `budget_dto.rs` 的 `#[path = "budget_dto_tests.rs"] mod budget_dto_tests;`
//! - `budget_guard_tests.rs` ← `budget_guard.rs` 的 `#[path = "budget_guard_tests.rs"] mod budget_guard_tests;`
//! - `budget_model_tests.rs` ← `budget_model.rs` 的 `#[path = "budget_model_tests.rs"] mod budget_model_tests;`

pub mod budget_bridge;
pub mod budget_dto;
pub mod budget_guard;
pub mod budget_handler;
pub mod budget_model;

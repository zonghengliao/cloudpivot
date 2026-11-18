//! ERP 合同域（P2-10 拆目录）：合同 CRUD、审批回调、联动 hook、PDF 导出、执行量统计。
//!
//! `contract_*.rs` 全部由本文件声明，可见性与拆前（挂在 `erp/mod.rs`）逐字一致：
//! 除 `contract_linkage` 是 `pub(crate)`（`flow::*` 要够得着）、测试模块是裸 `mod`
//! （`#[cfg(test)]`，只被 cargo test 自动发现，域外无代码引用）外，其余都是 `pub mod`。
//!
//! 注意 `contract_linkage_tests` 拆前就**不在**字母序声明块里，而是和 LNK-06/LNK-09 等
//! 兄弟测试一起放在 `erp/mod.rs` 的测试段 → 本文件末尾单列，别当成漏声明。

#[cfg(test)]
mod contract_approval_tests;
pub mod contract_bridge;
pub mod contract_dto;
/// M4-19 合同执行量统计的契约（已执行 / 合同额）：常量 / 纯判据 / 入出参。
pub mod contract_execution_dto;
/// M4-19 合同执行量统计的接口层：两个只读接口（总览 + 单合同下钻）。
pub mod contract_execution_handler;
/// M4-19 合同执行量统计的数据访问层：只读 V0458 的两个视图。
pub mod contract_execution_model;
pub mod contract_handler;
/// LNK-07 合同状态变更的联动 hook（唯一一份上下文 / 排除清单 / 幂等口径）。
pub(crate) mod contract_linkage;
pub mod contract_model;
/// 合同 PDF 导出（PDF-03）：条款正文 + 签章位 + 附件清单 + 骑缝章提示。
pub mod contract_pdf;

/// M4-19 合同执行量统计的真库用例（与其它 ERP 用例共用同一个串行锁）。
#[cfg(test)]
mod contract_execution_db_tests;

/// LNK-07 的真库单测（合同联动接线：审批通过触发一次、重复回调被抑制、
/// 排除动作确实不跑、人工流转触发）。只在 `cargo test` 时编译。
#[cfg(test)]
mod contract_linkage_tests;

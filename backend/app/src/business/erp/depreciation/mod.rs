//! 折旧域（FIN-2 / LNK-10）：折旧计提 / 红冲 / 接口 / 联动接线。
//!
//! 拆目录见 P2-18：原 `erp/depreciation_*.rs` 五个文件整体搬进本目录，模块路径由
//! `erp::depreciation_*` 变为 `erp::depreciation::depreciation_*`，文件内容与逻辑一字未改
//! （只改 `use` 路径）。
//!
//! * [`depreciation_model`] —— 折旧计提（期间闸 / 幂等闸 / 扫描 / 按部门多借方凭证 /
//!   回写卡片）+ 红冲 + 定时任务（`register_builtin`）；
//! * [`depreciation_handler`] —— 折旧 4 个接口：批次分页 / 详情 / 手动计提 / 红冲；
//! * [`depreciation_linkage`] —— LNK-10 的**唯一**联动接线（`pub(crate)`，两个宿主函数
//!   都在 [`depreciation_model`] 里）；
//! * `depreciation_dept_tests` / `depreciation_linkage_tests` —— 真库用例（仅 `cfg(test)`）。

pub mod depreciation_handler;
/// LNK-10 的**唯一**联动接线（过账 / 红冲两条路共用 `depreciation_model::run_batch` /
/// `reverse_batch` 内部的 `run_in_tx` / `run_deferred_after_commit`）。
///
/// `pub(crate)` 而不是私有：两个宿主函数都在 `depreciation_model` 里，但真库用例
/// （`depreciation_linkage_tests`）也要够得着同一份上下文口径。
pub(crate) mod depreciation_linkage;
pub mod depreciation_model;

/// E2E05-FIX-1：补提历史折旧按「期间归属部门」记费用（转移留痕还原）的真库用例。
#[cfg(test)]
mod depreciation_dept_tests;

/// LNK-10 的真库单测（折旧批次联动接线：过账触发一次、大批量只发一条（聚合）、
/// 重复计提不重复触发、红冲触发、排除动作确实不跑）。只在 `cargo test` 时编译。
#[cfg(test)]
mod depreciation_linkage_tests;

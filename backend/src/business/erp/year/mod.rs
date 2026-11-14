//! 年度域（YEAR-END）：年结 / 反年结 / 挡路明细 + 催办 + 联动接线。
//!
//! 拆目录见 P2-14：原 `erp/year_*.rs` 七个文件整体搬进本目录，模块路径由
//! `erp::year_end*` 变为 `erp::year::year_end*`，文件内容与逻辑一字未改。
//!
//! * [`year_end`] —— 年结主体（损益结转凭证 + 下年期初台账 + 反年结，五项前置校验）；
//! * [`year_end_linkage`] —— LNK-11 的唯一联动接线（`pub(crate)`，两个宿主函数都在 `year_end` 里）；
//! * [`year_end_remind`] —— 年结催办（复用 `period_remind` 的发送逻辑，只做上下文 + 取数 + 权限）；
//! * `year_end_test_support` / `*_tests` —— 真库用例的公共夹具与用例（仅 `cfg(test)`）。

pub mod year_end;
/// LNK-11 的**唯一**联动接线（年结 `close_year` / 反年结 `reopen_year` 两条边共用
/// `run_in_tx` / `run_deferred_after_commit`）。
///
/// `pub(crate)` 而不是私有：两个宿主函数都在 `year_end` 里，但真库用例
/// （`year_end_linkage_tests`）也要够得着同一份上下文口径。
pub(crate) mod year_end_linkage;
pub mod year_end_remind;

/// LNK-11 的真库单测（年结联动接线：年结触发一次、多张凭证的年度只发一条、重复年结被 409
/// 挡住不重复触发、反年结触发、事务回滚通知不发、排除动作确实不跑）。只在 `cargo test` 时编译。
#[cfg(test)]
mod year_end_linkage_tests;

#[cfg(test)]
pub mod year_end_test_support; // YEAR-END 真库用例的公共夹具（`2037-xx` / `2038-xx` / `YE-` 前缀）

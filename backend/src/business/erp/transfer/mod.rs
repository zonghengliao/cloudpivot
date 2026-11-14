//! ERP 调拨域（stock transfer）的模块声明 —— `transfer_*.rs` 的落点。
//!
//! 由 `erp/mod.rs` 原样搬出（P2-03 拆目录）：声明、doc 注释与 `#[cfg(test)]`
//! 一律保真，只换了落点。模块名不变，对外路径多了一层 `transfer`
//! （拆目录前直挂在 `crate::business::erp` 下，现在在 `crate::business::erp::transfer` 下）。

// ==================== 四件套 + 只读 + PDF ====================
pub mod transfer_approval_settle;
pub mod transfer_dto;
pub mod transfer_handler;
pub mod transfer_model;
pub mod transfer_overdue;
/// 调拨单 PDF 导出（PDF-04）：调出 / 调入仓库 + 双库位 + 原价转移成本口径。
pub mod transfer_pdf;

// ==================== TD-1 拆分出的部件（只经 facade 对外） ====================
// 公开 API 由 `transfer_model` facade 用 `pub use super::<部件>::*;` 重导出，
// 调用方路径与拆分前完全一致。
// 拆法见 `docs/refactor-methodology.md`，拆分器 `scripts/refactor/split_module.py`。
mod transfer_base; // 调拨单底座：常量 / 6 态状态机 / 行结构 / 取号
mod transfer_query; // 调拨单只读
mod transfer_scan; // 调拨单在途超期扫描
mod transfer_settle; // 调拨单过账结算
mod transfer_write; // 调拨单写入（建改删 + 状态流转）

// ==================== TD-2 拆分出的部件（只经 facade 对外） ====================
// `transfer_handler.rs`（4676 行，含 3249 行测试）按职责拆成 6 个代码部件 + 7 个测试部件，
// 公开 API 由 `transfer_handler` facade 用 `pub use super::<部件>::*;` 重导出，调用方路径不变。
// 拆法见 `docs/refactor-methodology.md`（TD-2 章节）。
mod transfer_handler_common; // 底座：错误出口（409/404/写库映射）/ 入参解析 / 明细预处理
mod transfer_handler_create; // 建单 / 复制 / 改单（取号 + 明细落库 + 草稿态校验）
mod transfer_handler_delete; // 软删（仅 DRAFT / CANCELLED）
mod transfer_handler_query; // 只读：分页 / 详情 / 涉及序列号 / 在途超期汇总
mod transfer_handler_status; // 六态状态机：审核 / 发出 / 在途 / 收货（含结算）/ 作废 + 审批发起
#[cfg(test)]
mod transfer_handler_test_support; // 测试支撑：把审批实例批到终态（真库用例共用）

// ==================== 联动接线 ====================
// LNK-01 的**唯一**联动接线（人工流转 + 审批回调两条路共用）。`pub(crate)` 而不是私有：
// 审批回调在 `flow::task_bridge` 里，跨模块必须够得着，才不至于让「配了已发布流程时
// STOCK_TRANSFER/APPROVED 的规则永远不触发」这条静默缺口复活。
pub(crate) mod transfer_linkage;

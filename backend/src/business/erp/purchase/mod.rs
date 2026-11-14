//! ERP 采购域（purchase）的模块声明 —— `purchase_*.rs` 的落点。
//!
//! 由 `erp/mod.rs` 原样搬出（P2-04 拆目录）：声明一律保真，只换了落点。
//! 模块名不变，对外路径多了一层 `purchase`
//! （拆目录前直挂在 `crate::business::erp` 下，现在在 `crate::business::erp::purchase` 下）。
//!
//! 两个成员**故意不在这里声明**（拆目录前就不在 `erp/mod.rs`，靠 `#[path]` 挂在各自父模块里）：
//!
//! - `purchase_linkage`：挂在 `purchase_model` 内（`#[path = "purchase_linkage.rs"]`），
//!   路径 `erp::purchase::purchase_model::purchase_linkage`；因与 `purchase_model` 同目录，
//!   `#[path]` 相对路径不用改。
//! - `purchase_linkage_db_tests`：挂在 `erp::stockin::stockin_handler` 内（真库用例），
//!   拆目录后该 `#[path]` 已随之改成 `../purchase/purchase_linkage_db_tests.rs`。

// ==================== 采购订单：四件套 + 信用守卫 + 审批桥 ====================
pub mod purchase_bridge;
pub mod purchase_credit;
pub mod purchase_dto;
pub mod purchase_handler;
pub mod purchase_model;

// ==================== 请购单（PR）：四件套 ====================
pub mod purchase_requisition_dto;
pub mod purchase_requisition_handler;
pub mod purchase_requisition_model;

// ==================== 采购退货：四件套 + 过账 ====================
pub mod purchase_return_bridge;
pub mod purchase_return_dto;
pub mod purchase_return_handler;
pub mod purchase_return_model;
pub mod purchase_return_posting;

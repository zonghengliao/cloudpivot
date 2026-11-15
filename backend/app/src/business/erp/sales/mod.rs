//! ERP 销售域（sales）的模块声明 —— `sales_*.rs` 的落点。
//!
//! 由 `erp/mod.rs` 原样搬出（P2-05 拆目录）：声明一律保真，只换了落点。
//! 模块名不变，对外路径多了一层 `sales`
//! （拆目录前直挂在 `crate::business::erp` 下，现在在 `crate::business::erp::sales` 下）。

// ==================== 销售链：订单 / 退货 / 目标 + 审批桥 ====================
pub mod sales_approval_bridge;
pub mod sales_model;
pub mod sales_order_handler;
pub mod sales_order_model;
pub mod sales_return_handler;
pub mod sales_return_model;
pub mod sales_return_posting;
pub mod sales_target_dto;
pub mod sales_target_handler;
pub mod sales_target_model;

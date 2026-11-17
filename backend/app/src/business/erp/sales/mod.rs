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

// ==================== 销售链：报价单（M3-02：CRUD + 状态机） ====================
pub mod quotation_approval_bridge;
pub mod quotation_handler;
pub mod quotation_model;
// ==================== 销售链：报价单 PDF（M3-07：复用 pdf 模块） ====================
pub mod quotation_pdf;

// ==================== 销售链：返利政策（M3-15：政策表 + 规则 按量/按额/阶梯） ====================
pub mod rebate_dto;
pub mod rebate_handler;
pub mod rebate_model;

// ==================== 销售链：返利结算（M3-16：按月度/季度算应返金额） ====================
pub mod rebate_settle_dto;
pub mod rebate_settle_handler;
pub mod rebate_settle_model;

// ==================== 销售链：返利红字发票（M3-17：结算单 → OUT_RED 票 + 红字凭证） ====================
pub mod rebate_invoice_dto;
pub mod rebate_invoice_handler;
pub mod rebate_invoice_model;

/// 返利政策的真库用例（`ERP_DB_URL` 开关；与其它 ERP 用例共用串行锁）。
#[cfg(test)]
mod rebate_db_tests;

/// 返利结算的真库用例（同上）。
#[cfg(test)]
mod rebate_settle_db_tests;

/// 返利红字发票的真库用例（同上）。
#[cfg(test)]
mod rebate_invoice_db_tests;

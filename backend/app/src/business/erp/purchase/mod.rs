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

// ==================== 采购计划（PP）：表结构（M4-01）+ CRUD/审批（M4-02）+ 转申请（M4-03） ====================
// M4-01 只交表结构：model 里是表名 / 列 / 取号（biz_seq PPL）/ 读入口 / 合计重算；
// M4-02 补上建单 / 改单 / 删单 / 提交审批（dto + handler + bridge），model 直接复用；
// M4-03 补上「按计划生成采购申请」（convert_dto + convert_handler，一仓一单 + 已执行量回写）。
// 执行率报表属 M4-04，前端页面属 M4-06 —— 本目录**不含**它们。
pub mod purchase_plan_bridge;
pub mod purchase_plan_convert_dto;
pub mod purchase_plan_convert_handler;
pub mod purchase_plan_dto;
pub mod purchase_plan_execution_dto;
pub mod purchase_plan_execution_handler;
pub mod purchase_plan_execution_model;
pub mod purchase_plan_handler;
pub mod purchase_plan_model;

// ==================== 框架协议（FW）：表结构（M4-07）+ 订单引用（M4-08）+ 执行量统计（M4-09） ====================
// M4-07 只交表结构：model 里是表名 / 列 / 取号（biz_seq FWA）/ 读入口 / 派生合计重算
// + 有效期与容差判据（纯函数）。
// M4-08 交「订单引用」：`framework_agreement_ref` 里是下单校验（可用性 / 单价 / 累计量与额）
// + 执行量重算回写 + 详情出参的协议引用；订单表侧的迁移见 V0451。
// M4-09 交「执行量统计」：`framework_agreement_execution_{dto,model,handler}` 里是
// 「已用 / 剩余」两个只读接口，由 V0452 的两个视图现算（不读回写列）。
// 系统单测属 M4-10、前端页面属 M4-11 —— 本目录**不含**它们。
pub mod framework_agreement_execution_dto;
pub mod framework_agreement_execution_handler;
pub mod framework_agreement_execution_model;
pub mod framework_agreement_model;
pub mod framework_agreement_ref;

/// 框架协议表结构的真库用例（M4-07；与其它 ERP 用例共用同一个串行锁）。
#[cfg(test)]
mod framework_agreement_db_tests;

/// 框架协议订单引用的真库用例（M4-08；同一个串行锁）。
#[cfg(test)]
mod framework_agreement_ref_db_tests;

/// 框架协议执行量统计的真库用例（M4-09；同一个串行锁）。
#[cfg(test)]
mod framework_agreement_execution_db_tests;

/// 采购计划执行率跟踪的真库用例（M4-04；同一个串行锁）。
#[cfg(test)]
mod purchase_plan_execution_db_tests;

/// 采购计划的真库用例（`ERP_DB_URL` 开关；与其它 ERP 用例共用同一个串行锁）。
#[cfg(test)]
mod purchase_plan_db_tests;

/// 采购计划 CRUD + 审批的真库用例（M4-02；同一个串行锁）。
#[cfg(test)]
mod purchase_plan_handler_db_tests;

/// 采购计划 → 采购申请转换的真库用例（M4-03；同一个串行锁）。
#[cfg(test)]
mod purchase_plan_convert_db_tests;

// ==================== 采购退货：四件套 + 过账 ====================
pub mod purchase_return_bridge;
pub mod purchase_return_dto;
pub mod purchase_return_handler;
pub mod purchase_return_model;
pub mod purchase_return_posting;

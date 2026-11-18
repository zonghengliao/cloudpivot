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
pub mod framework_agreement_dto;
pub mod framework_agreement_execution_dto;
pub mod framework_agreement_execution_handler;
pub mod framework_agreement_execution_model;
pub mod framework_agreement_flow;
pub mod framework_agreement_handler;
pub mod framework_agreement_model;
pub mod framework_agreement_ref;

// ==================== 供应商评估（SEV）：表结构（M4-12） ====================
// M4-12 只交表结构：model 里是表名 / 四维常量 / 权重与分数的合法判据 / 取号（biz_seq SEV）/
// 读入口 / 加权总分纯函数。总分在库里是**生成列**（写不进），Rust 侧 [`weighted_total`]
// 只用于试算与对拍。
// 评估打分编排 + 评级阈值属 M4-13、准入/黑名单属 M4-14、单测属 M4-15、页面属 M4-16 ——
// 本目录**不含**它们（零路由 / 零权限码 / 零菜单）。
pub mod supplier_evaluation_model;

// ==================== 供应商评估（SEV）：打分 + 评级（M4-13） ====================
// M4-13 交「打分与评级」的数据层：评级阈值（唯一出处）/ 总分→等级纯函数 /
// 打分落库（同期间重打分覆盖，不新建单号）/ 按阈值重评级（regrade）/
// 交期维度按真实入库单算准时率的参考分。接口与权限码属 M4-16。
pub mod supplier_evaluation_grading;

// ==================== 供应商准入 / 黑名单（M4-14） ====================
// 一张申请单承载三种动作（准入 / 拉黑 / 解除），走 `supplier_access` 审批流；
// 通过后落 `biz_supplier.status`（拉黑 → 停用，准入 / 解除 → 启用），
// 采购订单建单时按 `status = 1` 统一闸（409 / 66099）。表结构见 V0455。
pub mod supplier_access_dto;
pub mod supplier_access_flow;
pub mod supplier_access_handler;
pub mod supplier_access_model;

/// 供应商准入 / 黑名单的真库用例（M4-14；与其它 ERP 用例共用同一个串行锁）。
#[cfg(test)]
mod supplier_access_db_tests;

/// 供应商评估打分 + 评级的真库用例（M4-13；与其它 ERP 用例共用同一个串行锁）。
#[cfg(test)]
mod supplier_evaluation_grading_db_tests;

/// 供应商评估「单测」收口（M4-15：评级边界穷举 / 并发同期间 / 跨期重打分 / 大方量
/// regrade；与其它 ERP 用例共用同一个串行锁）。
#[cfg(test)]
mod supplier_evaluation_suite_tests;

/// 供应商评估表结构的真库用例（M4-12；与其它 ERP 用例共用同一个串行锁）。
#[cfg(test)]
mod supplier_evaluation_db_tests;

/// 框架协议表结构的真库用例（M4-07；与其它 ERP 用例共用同一个串行锁）。
#[cfg(test)]
mod framework_agreement_db_tests;

/// 框架协议订单引用的真库用例（M4-08；同一个串行锁）。
#[cfg(test)]
mod framework_agreement_ref_db_tests;

/// 框架协议执行量统计的真库用例（M4-09；同一个串行锁）。
#[cfg(test)]
mod framework_agreement_execution_db_tests;

/// 框架协议维护链路（建 / 改 / 删 / 提交 / 审批回调 / 下单后执行量涨）的真库用例
/// （M4-11b；同一个串行锁）。
#[cfg(test)]
mod framework_agreement_crud_db_tests;

/// 框架协议「超限反例」单测（M4-10；同一个串行锁）。
#[cfg(test)]
mod framework_agreement_over_limit_tests;

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

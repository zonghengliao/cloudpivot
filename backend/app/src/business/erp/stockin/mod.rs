//! ERP 收货入库域（stock-in）的模块声明 —— `stockin_*.rs` 的落点。
//!
//! 由 `erp/mod.rs` 原样搬出（P2-02 拆目录）：声明、doc 注释与 `#[cfg(test)]`
//! 一律保真，只换了落点。模块名不变，对外路径多了一层 `stockin`
//! （拆目录前直挂在 `crate::business::erp` 下，现在在 `crate::business::erp::stockin` 下）。

// ==================== 四件套 + 只读 ====================
pub mod stockin_bridge;
pub mod stockin_dto;
pub mod stockin_handler;
pub mod stockin_model;
pub mod stockin_query; // 收货入库只读（`POST_BIZ_TYPE` / `CANCEL_BIZ_TYPE` 是审批词表的登记点，见 doc_kind）

// ==================== TD-1 拆分出的部件（只经 facade 对外） ====================
// 公开 API 由 `stockin_model` facade 用 `pub use super::<部件>::*;` 重导出，
// 调用方路径与拆分前完全一致。
// 拆法见 `docs/refactor-methodology.md`，拆分器 `scripts/refactor/split_module.py`。
//
// `stockin_base` / `stockin_cancel` 用 `pub(in crate::business::erp)` 而不是裸 `mod`：
// `erp/cost/*`、`erp/stocktake/stocktake_model.rs`、`erp/sales/sales_return_posting.rs`、
// `erp/cancel_cascade.rs` 等**同域兄弟**按绝对路径取它们
// （`crate::business::erp::stockin::stockin_base::…`）。拆目录前那是「erp 内的私有」，
// 拆完写裸 `mod` 会缩成「stockin 内的私有」，兄弟模块取不到 ——
// `pub(in crate::business::erp)` 与拆前的可见范围逐字等价（不多不少）。
pub(in crate::business::erp) mod stockin_base; // 收货入库底座：常量 / 状态机 / 行结构 / 取号
pub(in crate::business::erp) mod stockin_cancel; // 收货入库作废链路
mod stockin_location; // 收货入库库位解析（P2-4：库位字典 ↔ 库位账行）
mod stockin_purchase; // 收货入库↔采购单联动
mod stockin_validate; // 收货入库校验（可作废前置）
pub(in crate::business::erp) mod stockin_write; // 收货入库写入（建改删 + 过账）

// ==================== TD-2 拆分出的部件（只经 facade 对外） ====================
// `stockin_handler.rs`（9394 行，含 5990 行测试）按职责拆成 7 个代码部件 + 13 个测试部件，
// 公开 API 由 `stockin_handler` facade 用 `pub use super::<部件>::*;` 重导出，调用方路径不变。
// 拆法见 `docs/refactor-methodology.md`（TD-2 章节）。
pub(in crate::business::erp) mod stockin_handler_cancel; // 作废：提交/撤回作废审批 + 审批后反冲
mod stockin_handler_common; // 底座：错误构造 / 行计划 / 快照与派生总额 / 锁与权限判定
mod stockin_handler_post; // 过账：提交审批 + 审批后过账 + 事务内扣库与成本
mod stockin_handler_purchase; // 采购联动：来源单解析 / 供应商守卫 / 行回填 / 过账前复查
mod stockin_handler_query; // 只读入口：分页 / 详情
mod stockin_handler_status; // 状态流转与其余 HTTP 入口
mod stockin_handler_write; // 写入：建单 / 复制 / 编辑 / 删除

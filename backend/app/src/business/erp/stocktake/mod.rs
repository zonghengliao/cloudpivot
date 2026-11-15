//! 盘点域（STOCK_TAKE · ARCH-3 DC-14）：账面快照 + 差异三本账 + 盘点差异凭证。
//!
//! 拆目录见 P2-15：原 `erp/stocktake_*.rs` 六个文件整体搬进本目录，模块路径由
//! `erp::stocktake_*` 变为 `erp::stocktake::stocktake_*`，文件内容与逻辑一字未改。
//!
//! * [`stocktake_model`] —— 盘点单数据层：账面快照 / 差异三本账（盘盈走入库、盘亏走出库）/
//!   盘点差异凭证 / 作废回滚；
//! * [`stocktake_dto`] —— 契约：建单（账面数服务端快照）/ 填实盘 / 提交审批 / 过账 / 作废 /
//!   详情 / 分页；
//! * [`stocktake_handler`] —— 6 个接口 + 过账 / 作废事务编排 + 审批发起（`stock_take_post`）；
//! * [`stocktake_bridge`] —— 审批回调（驳回 → 回草稿）+ 内控闸 + 反向兜底。
//!
//! `stocktake_linkage`（LNK-04 联动接线）**不在这里声明**：它的 `#[path]` 挂在
//! [`stocktake_handler`] 里（多 worker 并行时 `mod.rs` 是冲突热点），随文件一起搬过来即可。

pub mod stocktake_bridge;
pub mod stocktake_dto;
pub mod stocktake_handler;
pub mod stocktake_model;

/// 盘点单用例（纯逻辑 + 真库：盘盈 / 盘亏 / 无差异 / 作废回滚 / 审批流 / 负库存）。
#[cfg(test)]
pub mod stocktake_tests;

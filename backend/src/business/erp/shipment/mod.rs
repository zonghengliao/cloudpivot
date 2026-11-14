//! 发货域（销售链 · SO-08 / SO-09 / SO-10）：发货单数据层 / 接口层 / 过账库存效果。
//!
//! 拆目录见 P2-20：原 `erp/shipment_*.rs` 四个文件整体搬进本目录，模块路径由
//! `erp::shipment_*` 变为 `erp::shipment::shipment_*`，文件内容与逻辑一字未改
//! （只改 `use` 路径）。
//!
//! * [`shipment_model`] —— 发货单数据层（V0283）：`SH-yyyyMMdd-6 位` 取号 + 4 态状态域
//!   + **超发守卫**（未发量 = 订单 − 已发 − 未过账占用）；
//! * [`shipment_handler`] —— 发货单接口层：建单 / 详情 / 分页 / 状态流转 / 带出可发明细；
//! * [`shipment_posting`] —— 发货过账的库存效果（V0285）：销售出库单（`source_type=SALES`）
//!   → `inventory_writer` 三账负向 → `delivered_qty` → 订单 `PARTIAL_OUT`/`OUTBOUND`，
//!   **同一事务**。

pub mod shipment_handler;
pub mod shipment_model;
pub mod shipment_posting;

/// 发货单列表批量补齐（缺陷卡 t_45900ea7）：列表与详情逐字段同口径。只在 `cargo test` 时编译。
#[cfg(test)]
mod shipment_page_fill_tests;

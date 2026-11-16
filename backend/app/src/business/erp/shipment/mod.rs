//! 发货域（销售链 · SO-08 / SO-09 / SO-10）：发货单数据层 / 接口层 / 过账库存效果。
//!
//! 拆目录见 P2-20：原 `erp/shipment_*.rs` 四个文件整体搬进本目录，模块路径由
//! `erp::shipment_*` 变为 `erp::shipment::shipment_*`，文件内容与逻辑一字未改
//! （只改 `use` 路径）。
//!
//! * [`shipment_model`] —— 发货单数据层（V0283）：`SH-yyyyMMdd-6 位` 取号 + 4 态状态域
//!   + **超发守卫**（未发量 = 订单 − 已发 − 未过账占用）；
//! * [`shipment_handler`] —— 发货单接口层：建单 / 详情 / 分页 / 状态流转 / 带出可发明细；
//! * [`shipment_posting`] —— 发货过账的库存效果（M2-02）：销售出库单（`source_type=SALES`）
//!   落 `PENDING_POST`（出库待确认）→ 在途账 + `delivered_qty` → 订单
//!   `PARTIAL_OUT`/`OUTBOUND`，**同一事务**；三本账 / 凭证 / 应收在签收确认（M2-03）才动。
//! * [`shipment_transit`] —— 在途库存账（V0420）的写入口：发货量按「物料 × 发货仓 × 发货单」
//!   upsert，`qty_in_transit` 由库层生成列保证恒等。

pub mod shipment_handler;
pub mod shipment_model;
pub mod shipment_posting;
pub mod shipment_transit;

/// 发货单列表批量补齐（缺陷卡 t_45900ea7）：列表与详情逐字段同口径。只在 `cargo test` 时编译。
#[cfg(test)]
mod shipment_page_fill_tests;

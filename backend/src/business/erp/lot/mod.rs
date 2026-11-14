//! 批次主数据域（成本核算 · ARCH-9 §3.1 ① / §5.2 #7–#9）：`biz_lot` CRUD + 按物料取批号。
//!
//! 拆目录见 P2-21：原 `erp/lot_*.rs` 四个文件整体搬进本目录，模块路径由
//! `erp::lot_*` 变为 `erp::lot::lot_*`，文件内容与逻辑一字未改（只改 `use` 路径）。
//!
//! * [`lot_dto`] —— 契约：值域（与 V0370 的 CHECK 逐字一致）/ 列表筛选 / 建改入参
//!   （部分更新三态）/ 列表行 / 下拉选项 / 纯逻辑校验 + **唯一键冲突的友好文案**；
//! * [`lot_model`] —— 数据层：单头 + 2 个 join 补名（物料 / 供应商）+ 列表分页（unscoped）
//!   + **按物料取批号**（只要启用批）+ 建改落库；
//! * [`lot_handler`] —— 接口层：4 个 HTTP 入口（list / options / create / update）
//!   + 引用校验 + 重复预检。

pub mod lot_dto;
pub mod lot_handler;
pub mod lot_model;

/// B1-9 的真库用例（`ERP_DB_URL` 开关）：批次 CRUD + 按物料取批号 + 唯一键冲突文案。
#[cfg(test)]
mod lot_db_tests;

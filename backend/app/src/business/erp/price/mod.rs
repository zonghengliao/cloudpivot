//! 价格域（原 `erp/price_*.rs`，P2-08 拆目录）：取价解析 / 协议价与档位维护 / 询比价。
//!
//! 三个子域各自的定位（拆目录前写在 `erp/mod.rs` 的模块表里，逐字未改）：
//!
//! - [`price_dto`] / [`price_model`] / [`price_handler`] —— 取价解析（读）：
//!   三级粗筛 + 五级优先级，回答「此刻该多少」；
//! - [`price_admin_model`] / [`price_admin_handler`] —— 协议价 / 档位两表维护（写）；
//! - [`price_inquiry_dto`] / [`price_inquiry_model`] / [`price_inquiry_handler`] ——
//!   询比价（P2-01）：建单 / 录报价 / 定标 / 转采购订单。
//!
//! 原 `erp/mod.rs` 里这 8 条声明全部是 `pub mod`，拆目录后保持 `pub mod` 不变，
//! 对外路径由 `crate::business::erp::price::<模块>` 取。

pub mod price_admin_handler;
pub mod price_admin_model;
pub mod price_dto;
pub mod price_handler;
pub mod price_inquiry_dto;
pub mod price_inquiry_handler;
pub mod price_inquiry_model;
pub mod price_model;

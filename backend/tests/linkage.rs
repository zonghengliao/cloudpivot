//! 集成测试入口：跨模块联动矩阵。
//!
//! `cargo test --test linkage`（目标名 = 本文件名 `linkage`）。
//!
//! 用例按领域分文件：任务↔审批、收货链路、调拨链路、应付↔付款、凭证↔分录。
//! 夹具与连库细节见 [`common`]。

#[path = "linkage/mod.rs"]
mod linkage;

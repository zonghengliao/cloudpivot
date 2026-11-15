//! 客户档案域（销售链 · SO-01 / SO-02）：客户主档 CRUD + 信用额度 / 账期 + 取号 `CUS`。
//!
//! 拆目录见 P2-23：原 `erp/customer_*.rs` 两个文件整体搬进本目录，模块路径由
//! `erp::customer_*` 变为 `erp::customer::customer_*`，文件内容与逻辑一字未改
//! （只改 `mod` / `use` / intra-doc 链接）。
//!
//! * [`customer_model`] —— 客户档案数据层（V0260）：行结构 / 列清单 / `ACTIVE`·`FROZEN` 域、
//!   软删后编码可复用、分页（`scope::paginate_unscoped`）、取号 `CUS`、软删引用检查；
//! * [`customer_handler`] —— 客户接口层：5 个 HTTP 入口（建 / 改 / 软删 / 详情 / 分页）
//!   + 编码预览（只读不占号）+ 入口校验（信用额度 `>= 0`、账期、编码建后不可改）。

pub mod customer_handler;
pub mod customer_model;

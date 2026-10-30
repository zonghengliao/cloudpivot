//! ERP 模块（批次 D · W1）：主数据 / 仓储 / 库存的**数据层与接口契约**。
//!
//! 本卡（W1b）只落地两件事：
//!
//! | 文件 | 内容 |
//! |------|------|
//! | [`model`] | 13 张 `biz_*` 表的行结构、列清单、基础查询（含仓库子树、两级库存账） |
//! | [`dto`] | 每张表的建 / 改 / 筛选 / 卡片 / 详情契约 + 校验（`Decimal` 定点金额） |
//!
//! `handler.rs`（路由 / 权限码 / 事务编排 / 单号取号）留给 **W1c**，本模块故意不引用任何
//! `axum` 类型，避免 W1c 之前就出现半成品的路由与权限面。
//!
//! # 表结构的唯一依据
//!
//! `backend/migrations/business/V0137__erp_master_data_and_warehouse.up.sql`。
//! 卡面与架构文档在四处与真实库不一致（`warehouse_path` 列名、
//! `idx_biz_warehouse_path_pattern` 索引名、`serial_required` 属 V0138、
//! 以及读接口是否吃 `&Transaction`），差异与处置写在 [`model`] 的模块文档里。
//!
//! # 数据权限
//!
//! 13 张表都是全局主数据 / 仓储结构（V0137 无 `dept_id` / 归属人列），
//! 分页一律走 `scope::paginate_unscoped` + `ScopeTarget::unscoped`，
//! 理由见 [`model`] 文件头的 `SCOPE-EXEMPT` 说明。

pub mod dto;
pub mod model;

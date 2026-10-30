//! ERP 模块（批次 D · W1）：主数据 / 仓储 / 库存的**数据层 + 契约 + 接口层**。
//!
//! | 文件 | 内容 | 落地批次 |
//! |------|------|----------|
//! | [`model`] | 13 张 `biz_*` 表的行结构、列清单、基础查询（含仓库子树、两级库存账） | W1b |
//! | [`dto`] | 每张表的建 / 改 / 筛选 / 卡片 / 详情契约 + 校验（`Decimal` 定点金额） | W1b |
//! | [`handler`] | 44 个接口：5 张主数据表的 CRUD + 3 个特殊查询 + 8 张台账的列表/详情 | **W1c** |
//!
//! # 表结构的唯一依据
//!
//! `backend/migrations/business/V0137__erp_master_data_and_warehouse.up.sql`。
//! 卡面与架构文档在四处与真实库不一致（`warehouse_path` 列名、
//! `idx_biz_warehouse_path_pattern` 索引名、`serial_required` 属 V0138、
//! 以及读接口是否吃 `&Transaction`），差异与处置写在 [`model`] 的模块文档里；
//! W1c 又发现**卡面点名的 11 张表里有 6 张 V0137 根本没建**（见 [`handler`] 文件头），
//! 接口按真实存在的 13 张表给。
//!
//! # 权限码
//!
//! 24 个卡面码 + 8 个只读码 = 32 个，全部由
//! `backend/migrations/business/V0141__erp_permissions.up.sql` 种进 `sys_menu.auth_code`
//! （**没有 `sys_permission` 表**：权限码的唯一来源就是 `sys_menu.auth_code`）。
//! 码值清单在 [`handler::CARD_PERMISSIONS`] / [`handler::READONLY_PERMISSIONS`]，
//! 与迁移里的 `VALUES` 列表逐字对应（对不上时 `scripts/verify/w1c_erp_api.sh` 会红）。
//!
//! # 数据权限
//!
//! 13 张表都是全局主数据 / 仓储结构（V0137 无 `dept_id` / 归属人列），
//! 分页一律走 `scope::paginate_unscoped` + `ScopeTarget::unscoped`，
//! 理由见 [`model`] 文件头的 `SCOPE-EXEMPT` 说明。

pub mod dto;
pub mod handler;
pub mod model;

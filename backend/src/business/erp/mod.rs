//! ERP 模块（批次 D · W1）：主数据 / 仓储 / 库存的**数据层 + 契约 + 接口层**。
//!
//! | 文件 | 内容 | 落地批次 |
//! |------|------|----------|
//! | [`model`] | 13 张 `biz_*` 表的行结构、列清单、基础查询（含仓库子树、两级库存账） | W1b |
//! | [`dto`] | 每张表的建 / 改 / 筛选 / 卡片 / 详情契约 + 校验（`Decimal` 定点金额） | W1b |
//! | [`handler`] | 44 个接口：5 张主数据表的 CRUD + 3 个特殊查询 + 8 张台账的列表/详情 | **W1c** |
//! | [`transfer_model`] | 库存调拨单（6 态状态机 + 取号 + 审批接入）与明细/序列号子表 | **W3b** |
//! | [`transfer_dto`] | 调拨单出入参契约（建单 / 改单 / 状态流转 / 详情 / 序列号分组） | **W3b** |
//! | [`transfer_handler`] | 调拨单 7 个接口：CRUD + 状态流转 + 审批发起/回调 | **W3b** |
//! | [`transfer_overdue`] | 在途超期扫描与告警（每小时；阈值走 `sys_config`，默认 7 天） | **LNK-F12** |
//! | [`purchase_model`] | 采购订单数据层：两种金额口径（EXCL/INCL）+ 4 边状态机 + 取号 | **P2-A2** |
//! | [`purchase_dto`] | 采购订单出入参契约：建单 / 改单 / 状态流转 / 详情 + 校验 | **P2-A2** |
//! | [`purchase_handler`] | 采购订单 4 个写接口：建 / 改 / 状态流转 / 删 | **P2-A2** |
//! | [`stockin_model`] | 收货入库数据层：**过账**（移动加权加法 + 库存流水 + 一物一码）+ 取号 | **P2-B1** |
//! | [`stockin_dto`] | 收货入库出入参契约：建单 / 改单 / 过账 / 详情 / 分页 + 校验 | **P2-B1** |
//! | [`stockin_handler`] | 收货入库 6 个接口：建 / 改 / 过账 / 作废 / 删 / 列表 + 详情 | **P2-B1** |
//! | [`payable`] | 应付单数据层：过账即生成（AP-yyyyMMdd-6 位）+ 作废联动 | **P2-C1** |
//! | [`voucher`] | 凭证 / 分录 / 科目映射规则：借贷平衡 + 红冲（借贷互换） | **P2-C1** |
//! | [`voucher_query`] | 凭证查询：分页 + 详情（含全部分录），只读 | **P2-D1** |
//! | [`ledger`] | 总账流水（逐行余额）+ 科目余额表（Σ借 = Σ贷 校验），只读 | **P2-D1** |
//! | [`payable_query`] | 应付单查询：分页 + 详情（含关联凭证号），只读 | **P2-C2** |
//! | [`payable_payment`] | 应付付款登记（部分付款累计 → PARTIAL_PAID/SETTLED）+ 应付作废（联动红冲） | **P7** |
//! | [`voucher_write`] | 凭证红冲（复用 `voucher::reverse_posted`）+ 手工新增凭证（分录手工录入） | **P7** |
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
pub mod inventory_audit;
pub mod ledger;
pub mod model;
pub mod payable;
pub mod payable_payment;
pub mod payable_query;
pub mod purchase_dto;
pub mod purchase_handler;
pub mod purchase_model;
pub mod serial_dto;
pub mod serial_handler;
pub mod serial_model;
pub mod stockin_dto;
pub mod stockin_handler;
pub mod stockin_model;
pub mod transfer_dto;
pub mod transfer_handler;
pub mod transfer_model;
pub mod transfer_overdue;
pub mod voucher;
pub mod voucher_query;
pub mod voucher_write;

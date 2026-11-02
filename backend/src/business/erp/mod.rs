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
//! | [`purchase_handler`] | 采购订单 4 个写接口：建 / 改 / 状态流转 / 删 + **提交审批**（3A） | **P2-A2 / P-FLOW-3A** |
//! | [`stockin_model`] | 收货入库数据层：**过账**（移动加权加法 + 库存流水 + 一物一码）+ 取号 | **P2-B1** |
//! | [`stockin_dto`] | 收货入库出入参契约：建单 / 改单 / 过账 / 详情 / 分页 + 校验 | **P2-B1** |
//! | [`stockin_handler`] | 收货入库 6 个接口：建 / 改 / 过账 / 作废 / 删 / 列表 + 详情 | **P2-B1** |
//! | [`stockin_bridge`] | 收货单**作废审批** / **过账审批**的业务回调 + 反向兜底（孤儿实例回收） | **P16 / P-FLOW-3A** |
//! | [`stockout_model`] | 出库单数据层（ARCH-3 DC-13）：取号 + 三本账**负向**（成本账 / 库位账 / 流水）+ 负库存闸 + 作废回滚 | **ARCH-3 P0** |
//! | [`stockout_dto`] | 出库单出入参契约：建单 / 改单 / 状态流转（提交 / 过账 / 作废）/ 详情 / 分页 + 校验 | **ARCH-3 P0** |
//! | [`stockout_handler`] | 出库单 6 个接口：建 / 改 / 删 / 状态流转 / 列表 / 详情 + 过账与作废的事务编排 | **ARCH-3 P0** |
//! | [`purchase_bridge`] | 采购订单**审批**的业务回调 + 内控闸 + 反向兜底 | **P-FLOW-3A** |
//! | [`payable`] | 应付单数据层：过账即生成（AP-yyyyMMdd-6 位）+ 作废联动 | **P2-C1** |
//! | [`voucher`] | 凭证 / 分录 / 科目映射规则：借贷平衡 + 红冲（借贷互换） | **P2-C1** |
//! | [`voucher_query`] | 凭证查询：分页 + 详情（含全部分录），只读 | **P2-D1** |
//! | [`ledger`] | 总账流水（逐行余额）+ 科目余额表（Σ借 = Σ贷 校验），只读 | **P2-D1** |
//! | [`payable_query`] | 应付单查询：分页 + 详情（含关联凭证号），只读 | **P2-C2** |
//! | [`payable_payment`] | 应付付款登记（部分付款累计 → PARTIAL_PAID/SETTLED）+ 应付作废（联动红冲）+ **可付信息**（D5：`GET /payable/{id}/payment-info`，详情抽屉「发起付款」的唯一读数） | **P7 / D5** |
//! | [`payable_bridge`] | 应付**付款审批** / **作废审批**的业务回调 + 内控闸（制单人不能批自己）+ 反向兜底 | **P17 / P-FLOW-3B** |
//! | [`voucher_write`] | 凭证红冲（复用 `voucher::reverse_posted`）+ 手工新增凭证（分录手工录入）+ **红冲审批**（P-FLOW-3B） | **P7 / P-FLOW-3B** |
//! | [`voucher_bridge`] | 凭证**红冲审批**的业务回调 + 内控闸（制单人不能批自己）+ 反向兜底 | **P-FLOW-3B** |
//! | [`report`] | 资产负债表：报表项↔科目映射（`biz_report_item`）+ 勾稽校验（资产 ≡ 负债+权益） | **ERP-R1** |
//! | [`income_statement`] | 利润表：损益类**发生额**（本期 / 本年累计）+ 两表勾稽（净利润 ≡ 未分配利润变动） | **ERP-R2** |
//! | [`cash_flow`] | 现金流量表（**简化版**）：按现金收支流水 + 对方科目分经营 / 投资 / 筹资 + 勾稽货币资金 | **ERP-R3** |
//! | [`opening`] | 期初余额录入（方式 A 按科目 + 库存期初）→ 生成 / 覆盖唯一的期初凭证 | **ERP-R5** |
//! | [`year_end`] | 年结：损益结转凭证（`YEAR_END`）+ 下年期初台账 + 反年结（五项前置校验） | **ERP-R4** |
//! | [`batch`] | **批量操作（D7）**：勾选 → 逐条独立事务 → 逐条报告（过账 / 作废 / 删除 / 提交审批 / 导出） | **D7** |
//! | [`contract_model`] | 合同数据层（**A1 基座 + A2 CRUD**）：乙方引用解析（无外键靠应用层校验）、状态常量 / 边表、取号、分页、建改删 | **A1 / A2** |
//! | [`contract_dto`] | 合同出入参契约：筛选 / 新建 / 修改 / 状态流转 / 列表行 / 详情 + 纯逻辑校验 | **A2** |
//! | [`contract_handler`] | 合同 6 个接口：分页 / 详情 / 新建 / 修改 / 状态流转 / 软删 | **A1 装配位 → A2 实现** |
//! | [`unit_guard`] | 单据单位止血（DC-09）：明细的物料基础单位必须是 `is_base = 1`，否则 400 + 折算建议 | **ARCH-3 P0** |
//! | [`approval_gate`] | 审批流未配置改「默认拒绝」（DC-01）：6 个入口 409 + 新码、显式跳过（`business:erp:skip-approval` + 理由 ≥10 字）、8 个流程配置自检端点 | **ARCH-3 P0** |
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

pub mod approval_gate;
pub mod batch;
pub mod cash_flow;
pub mod contract_dto;
pub mod contract_handler;
pub mod contract_model;
pub mod dto;
pub mod form_snapshot;
pub mod handler;
pub mod income_statement;
pub mod inventory_audit;
pub mod ledger;
pub mod model;
pub mod opening;
pub mod payable;
pub mod payable_bridge;
pub mod payable_payment;
pub mod payable_query;
pub mod period;
pub mod purchase_bridge;
pub mod purchase_dto;
pub mod purchase_handler;
pub mod purchase_model;
pub mod report;
pub mod separation;
pub mod serial_dto;
pub mod serial_handler;
pub mod serial_model;
pub mod stockin_bridge;
pub mod stockin_dto;
pub mod stockin_handler;
pub mod stockin_model;
pub mod stockout_dto;
pub mod stockout_handler;
pub mod stockout_model;
pub mod transfer_dto;
pub mod transfer_handler;
pub mod transfer_model;
pub mod transfer_overdue;
pub mod unit_guard;
pub mod voucher;
pub mod voucher_bridge;
pub mod voucher_query;
pub mod voucher_write;
pub mod year_end;

// ==================== TD-1 拆分出的部件（只经 facade 对外） ====================
// 下面 18 个 `mod` 是三个超千行 model 文件按职责拆出的部件，**不直接对外**：
// 公开 API 由 `model` / `stockin_model` / `transfer_model` 三个 facade 用
// `pub use super::<部件>::*;` 重导出，调用方路径与拆分前完全一致。
// 拆法见 `docs/refactor-methodology.md`，拆分器 `scripts/refactor/split_module.py`。
mod model_base; // 底座：分页归一化 / id 解析 / 通用取数模板
mod model_inventory; // 库存台账只读
mod model_master; // 主数据只读（单位/仓库/库位/物料/供应商/科目/期间/取号规则）
mod model_order; // 采购订单只读（头 + 明细）
mod stockin_base; // 收货入库底座：常量 / 状态机 / 行结构 / 取号
mod stockin_cancel; // 收货入库作废链路
mod stockin_purchase; // 收货入库↔采购单联动
mod stockin_query; // 收货入库只读
mod stockin_validate; // 收货入库校验（可作废前置）
mod stockin_write; // 收货入库写入（建改删 + 过账）
mod stockout_base; // 出库单底座：常量 / 状态机 / 行结构 / 取号
mod transfer_base; // 调拨单底座：常量 / 6 态状态机 / 行结构 / 取号
mod transfer_query; // 调拨单只读
mod transfer_scan; // 调拨单在途超期扫描
mod transfer_settle; // 调拨单过账结算
mod transfer_write; // 调拨单写入（建改删 + 状态流转）

/// 夹具卫生（`#[cfg(test)]`）：`biz_status_log` 孤儿行的统一判据与清理。
#[cfg(test)]
pub(crate) mod fixture_hygiene;
/// P-FLOW-3B 的真库单测（应付作废 + 凭证红冲审批）。只在 `cargo test` 时编译。
#[cfg(test)]
mod pflow3b_tests;

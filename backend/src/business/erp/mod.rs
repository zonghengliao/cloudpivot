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
//! | [`purchase_credit`] | 供应商信用额度**现算**守卫（提交时点快照 + 强制审批） | **ARCH-3 P2-2** |
//! | [`purchase_dto`] | 采购订单出入参契约：建单 / 改单 / 状态流转 / 详情 + 校验 | **P2-A2** |
//! | [`purchase_handler`] | 采购订单 4 个写接口：建 / 改 / 状态流转 / 删 + **提交审批**（3A） | **P2-A2 / P-FLOW-3A** |
//! | [`stockin_model`] | 收货入库数据层：**过账**（移动加权加法 + 库存流水 + 一物一码）+ 取号 | **P2-B1** |
//! | [`stockin_dto`] | 收货入库出入参契约：建单 / 改单 / 过账 / 详情 / 分页 + 校验 | **P2-B1** |
//! | [`stockin_handler`] | 收货入库 6 个接口：建 / 改 / 过账 / 作废 / 删 / 列表 + 详情 | **P2-B1** |
//! | [`stockin_bridge`] | 收货单**作废审批** / **过账审批**的业务回调 + 反向兜底（孤儿实例回收） | **P16 / P-FLOW-3A** |
//! | [`stockout_model`] | 出库单数据层（ARCH-3 DC-13）：取号 + 三本账**负向**（成本账 / 库位账 / 流水）+ 负库存闸 + 作废回滚 | **ARCH-3 P0** |
//! | [`stockout_dto`] | 出库单出入参契约：建单 / 改单 / 状态流转（提交 / 过账 / 作废）/ 详情 / 分页 + 校验 | **ARCH-3 P0** |
//! | [`stockout_handler`] | 出库单 6 个接口：建 / 改 / 删 / 状态流转 / 列表 / 详情 + 过账与作废的事务编排 | **ARCH-3 P0** |
//! | [`stocktake_model`] | 盘点单数据层（ARCH-3 DC-14）：账面快照 + 差异三本账（盘盈走入库 / 盘亏走出库）+ 盘点差异凭证 + 作废回滚 | **ARCH-3 P1** |
//! | [`stocktake_dto`] | 盘点单契约：建单（账面数服务端快照）/ 填实盘 / 提交审批 / 过账 / 作废 / 详情 / 分页 | **ARCH-3 P1** |
//! | [`stocktake_handler`] | 盘点单 6 个接口 + 过账 / 作废事务编排 + 审批发起（`stock_take_post`） | **ARCH-3 P1** |
//! | [`stocktake_bridge`] | 盘点**审批**的业务回调（驳回 → 回草稿）+ 内控闸 + 反向兜底 | **ARCH-3 P1** |
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
//! | [`payable_aging`] | 应付账款**账龄分析**：按到期日分 未到期 / 1-30 / 31-60 / 61-90 / 90+ 五档 + 按供应商汇总 + 明细 | **P1-FIX-8A** |
//! | [`report_item_admin`] | 报表项**维护面**（HTTP）：列表 / 新增 / 编辑 / 排序 / 启停 / 取数预览 + 整表公式校验 | **P1-FIX-8B** |
//! | [`report_item_model`] | `biz_report_item` 的数据访问层（含停用行、引用关系、科目目录） | **P1-FIX-8B** |
//! | [`opening`] | 期初余额录入（方式 A 按科目 + 库存期初）→ 生成 / 覆盖唯一的期初凭证 | **ERP-R5** |
//! | [`year_end`] | 年结：损益结转凭证（`YEAR_END`）+ 下年期初台账 + 反年结（五项前置校验） | **ERP-R4** |
//! | [`batch`] | **批量操作（D7）**：勾选 → 逐条独立事务 → 逐条报告（过账 / 作废 / 删除 / 提交审批 / 导出） | **D7** |
//! | [`contract_model`] | 合同数据层（**A1 基座 + A2 CRUD**）：乙方引用解析（无外键靠应用层校验）、状态常量 / 边表、取号、分页、建改删 | **A1 / A2** |
//! | [`contract_bridge`] | 合同**审批**的业务回调（通过 → 已签订 / 驳回 → 回草稿）+ 内控 + 反向兜底 | **ARCH-3 R-P1-1** |
//! | [`contract_dto`] | 合同出入参契约：筛选 / 新建 / 修改 / 状态流转 / 列表行 / 详情 + 纯逻辑校验 | **A2** |
//! | [`contract_handler`] | 合同 6 个接口：分页 / 详情 / 新建 / 修改 / 状态流转 / 软删 | **A1 装配位 → A2 实现** |
//! | [`asset_compute`] | 固定资产**纯计算**（ARCH-6 §5.3/§5.4/§5.6.4）：起提日 / 月折旧 / 尾差 / 期间工具 / 处置分录（不碰库） | **FIN-2** |
//! | [`asset_dto`] | 固资契约：分类 / 卡片 / 转移 / 处置 / 折旧批次的入参与校验（纯函数，必填缺 400 指名） | **FIN-2** |
//! | [`asset_model`] | 固资数据层（V0229/V0230）：分类 / 卡片 / 变动留痕（append-only）/ 折旧批次与明细 + 扫描 SQL | **FIN-2** |
//! | [`asset_handler`] | 固资 14 个接口：分类 CRUD、建卡（含购入凭证）、转移 / 变更使用人、处置提交 / 作废、详情 | **FIN-2** |
//! | [`asset_bridge`] | 资产**处置审批**的业务回调（通过 → 处置凭证 + 卡片 DISPOSED + 留痕）+ 内控闸 | **FIN-2** |
//! | [`depreciation_model`] | 折旧计提（期间闸 / 幂等闸 / 扫描 / 按部门多借方凭证 / 回写卡片）+ 红冲 + 定时任务 | **FIN-2** |
//! | [`depreciation_handler`] | 折旧 4 个接口：批次分页 / 详情 / 手动计提 / 红冲 | **FIN-2** |
//! | [`expense_compute`] | 报销**纯计算**（ARCH-6 §4.5）：价税拆分 / 汇总 / 凭证行 / 金额分级（不碰库） | **FIN-3** |
//! | [`expense_dto`] | 报销契约：费用类型 / 报销单 / 明细 / 付款 / 作废的入参与校验 | **FIN-3** |
//! | [`expense_model`] | 报销数据层（V0242）：类型 / 单据 / 明细（整体重建）/ 付款记录 + 行锁 | **FIN-3** |
//! | [`expense_handler`] | 报销 15 个接口：类型 CRUD、建改删查提交撤回作废付款 | **FIN-3** |
//! | [`expense_bridge`] | 报销**审批**回调（通过 → 计提凭证 + usage CONSUMED）+ 内控闸 + 付款/红冲凭证 | **FIN-3** |
//! | [`voucher_multi`] | **多借方凭证入口**（ARCH-6 §6.4 边界 ①）：折旧按部门拆借方、购入价税拆分、处置三张凭证都走它 | **FIN-2** |
//! | [`unit_guard`] | 单据单位止血（DC-09）：明细的物料基础单位必须是 `is_base = 1`，否则 400 + 折算建议 | **ARCH-3 P0** |
//! | [`approval_gate`] | 审批流未配置改「默认拒绝」（DC-01）：6 个入口 409 + 新码、显式跳过（`business:erp:skip-approval` + 理由 ≥10 字）、8 个流程配置自检端点 | **ARCH-3 P0** |
//! | [`reminder`] | **业务提醒任务包**（DC-06 / DC-12）：4 类提醒的元数据 / 业务日 / 幂等台账（`biz_reminder_log`）/ 收件人解析 / 统一发送 / 4 个 cron 注册 / 手动触发端点 | **ARCH-3 R-P1-2** |
//! | [`reminder_payable`] | ① 应付到期：`due_date` 前 7/3/1 天 → 供应商对应采购员 + 财务（经理/总监） | **ARCH-3 R-P1-2** |
//! | [`reminder_purchase`] | ② 采购超期：超过 `expect_date` 未收完 → 制单人 + 采购经理（重提醒 3 天） | **ARCH-3 R-P1-2** |
//! | [`reminder_contract`] | ③ 合同到期：`end_date` 前 30/15/7/1 天 → 制单人 + 负责人 + 采购经理 | **ARCH-3 R-P1-2** |
//! | [`reminder_stock`] | ④ 低库存：`qty < safety_stock`（DC-12 摆设字段落地）→ 仓管主管 | **ARCH-3 R-P1-2** |
//! | [`customer_model`] | 客户档案数据层（V0260）：`ACTIVE`/`FROZEN` 域、软删后编码可复用、分页 | **SO-01/SO-02** |
//! | [`sales_model`] | 销售订单数据层（V0264）：`SO-yyyyMMdd-6 位` 取号 + 7 态状态域 + 金额恒等守卫 | **SO-04** |
//! | [`shipment_model`] | 发货单数据层（V0283）：`SH-yyyyMMdd-6 位` 取号 + 4 态状态域 + **超发守卫**（未发量 = 订单 − 已发 − 未过账占用） | **SO-08** |
//! | [`shipment_posting`] | 发货过账的库存效果（V0285）：销售出库单（`source_type=SALES`）→ `inventory_writer` 三账负向 → `delivered_qty` → 订单 `PARTIAL_OUT`/`OUTBOUND`，**同一事务** | **SO-10** |
//! | [`receivable`] | 应收单写侧（V0287）：手工挂账（`AR-yyyyMMdd-000001` / MANUAL 来源）+ 坏账核销（`WRITTEN_OFF` + 必须给原因） | **SO-13** |
//! | [`receivable_query`] | 应收查询：分页 + 详情（含客户名 / 关联凭证号），只读 | **SO-13** |
//! | [`receivable_aging`] | 应收账款**账龄分析**：分档与 `payable_aging` **同源**（同一份 BUCKETS / bucket_of / bucket_case_sql） | **SO-13** |
//! | [`receivable_bridge`] | 销售出库过账 → **自动挂应收**（`source_type='SALES_OUT'`，与库存扣减 / 成本结转凭证同一事务；幂等靠 `uk_biz_ar_source`） | **SO-14** |
//! | [`receipt`] | 收款单写侧 + 读侧（V0292）：登记（`RC-yyyyMMdd-000001`）/ 核销到多张应收（回写 `received_amount`+状态）/ 确认 / 过账（借 1002 / 贷 1122）/ 作废 | **SO-17** |
//! | [`receipt_refund`] | 收款退款单（V0294/V0295）：登记（`RR-yyyyMMdd-000001`）/ 核销冲减应收已收（`received_amount -= 金额`，应收退回 `PARTIAL`/`OPEN`）/ 确认 / 过账（借 1122 / 贷 1002）/ 草稿作废 —— SO-14 遗留「已收款的应收作废出库单」的前置单据 | **SO-14 遗留** |
//! | [`invoice`] | 销售发票写侧 + 读侧（V0298）：开票（`IV-yyyyMMdd-000001` / 从应收带金额 / 不超过应收）/ 开票生成收入凭证（借 1122 / 贷 5001 + 2221.02）并回写应收 / 归档 / 作废（红冲 + 撤回应收发票号） | **SO-20** |
//! | [`commission_dto`] | 佣金结算契约：计提入参 / 结算单行 / 明细行 / 状态迁移（`DRAFT → APPROVED → PAID`） | **SO-36** |
//! | [`commission_model`] | 佣金数据层（V0309/V0311）：候选应收 → 已认领基数、规则快照、结算单取号（`CM-yyyyMM`） | **SO-36** |
//! | [`commission_handler`] | 佣金 4 个接口：计提（`settle`）/ 分页 / 详情 / 审批发放 + 纯函数 `pick_rule` / `commission_of` | **SO-36** |
//! | [`commission_rule_dto`] | 佣金规则契约（V0309 §②）：范围 / 计算方式 / 比率金额校验 + 部分更新三态合并 | **SO-36b** |
//! | [`commission_rule_model`] | 佣金规则数据层（`biz_commission_rule` 17 列）：分页（含 `scope_name` 派生列）/ 建改软删 / 引用计数 | **SO-36b** |
//! | [`commission_rule_handler`] | 佣金规则 4 个接口：分页 / 新建 / 编辑（停用）/ 软删（`SO-37` 前端依赖） | **SO-36b** |
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
pub mod asset_bridge;
pub mod asset_compute;
pub mod asset_dto;
pub mod asset_handler;
pub mod asset_model;
pub mod batch;
pub mod budget_bridge;
pub mod budget_dto;
pub mod budget_guard;
pub mod budget_handler;
pub mod budget_model;
pub mod cancel_cascade;
pub mod cash_flow;
pub mod commission_dto;
pub mod commission_handler;
pub mod commission_model;
pub mod commission_rule_dto;
pub mod commission_rule_handler;
pub mod commission_rule_model;
#[cfg(test)]
mod contract_approval_tests;
pub mod contract_bridge;
pub mod contract_dto;
pub mod contract_handler;
pub mod contract_model;
pub mod credit;
pub mod currency;
pub mod currency_admin_handler;
pub mod currency_admin_model;
pub mod customer_handler;
pub mod customer_model;
pub mod depreciation_handler;
pub mod depreciation_model;
pub mod dto;
pub mod expense_bridge;
pub mod expense_compute;
pub mod expense_dto;
pub mod expense_handler;
pub mod expense_model;
pub mod form_snapshot;
pub mod fx_revaluation;
pub mod handler;
pub mod income_statement;
pub mod inventory_audit;
/// 库存三本账（`biz_inventory` / `biz_inventory_location` / `biz_inventory_txn`）的**唯一写入口**。
///
/// P2-5A 收口：所有写 SQL 住在这里，业务模块只编排；闸门在
/// `scripts/verify/inventory_write_gate.py`（非测试代码出现直接写 => 判红）。
mod inventory_writer;
pub mod invoice;
pub mod ledger;
pub mod model;
pub mod opening;
pub mod payable;
pub mod payable_aging;
pub mod payable_bridge;
pub mod payable_payment;
pub mod payable_query;
pub mod period;
pub mod period_items;
pub mod period_owner;
pub mod period_remind;
pub mod price_admin_handler;
pub mod price_admin_model;
pub mod price_dto;
pub mod price_handler;
pub mod price_model;
pub mod purchase_bridge;
pub mod purchase_credit;
pub mod purchase_dto;
pub mod purchase_handler;
pub mod purchase_model;
pub mod receipt;
pub mod receipt_refund;
pub mod receipt_refund_bridge;
pub mod receivable;
pub mod receivable_aging;
pub mod receivable_bridge;
pub mod receivable_query;
pub mod reminder;
pub mod reminder_contract;
pub mod reminder_payable;
pub mod reminder_purchase;
pub mod reminder_stock;
pub mod report;
pub mod report_item_admin;
pub mod report_item_model;
pub mod sales_approval_bridge;
pub mod sales_model;
pub mod sales_order_handler;
pub mod sales_order_model;
pub mod sales_return_handler;
pub mod sales_return_model;
pub mod sales_return_posting;
pub mod separation;
pub mod serial_dto;
pub mod serial_handler;
pub mod serial_model;
pub mod shipment_handler;
pub mod shipment_model;
pub mod shipment_posting;
pub mod stockin_bridge;
pub mod stockin_dto;
pub mod stockin_handler;
pub mod stockin_model;
pub mod stockout_dto;
pub mod stockout_handler;
pub mod stockout_model;
pub mod stocktake_bridge;
pub mod stocktake_dto;
pub mod stocktake_handler;
pub mod stocktake_model;
pub mod transfer_dto;
pub mod transfer_handler;
pub mod transfer_model;
pub mod transfer_overdue;
pub mod unit_guard;
pub mod voucher;
pub mod voucher_bridge;
pub mod voucher_multi;
pub mod voucher_query;
pub mod voucher_write;
pub mod year_end;
pub mod year_end_remind;

// ==================== TD-1 拆分出的部件（只经 facade 对外） ====================
// 下面 18 个 `mod` 是三个超千行 model 文件按职责拆出的部件，**不直接对外**：
// 公开 API 由 `model` / `stockin_model` / `transfer_model` 三个 facade 用
// `pub use super::<部件>::*;` 重导出，调用方路径与拆分前完全一致。
// 拆法见 `docs/refactor-methodology.md`，拆分器 `scripts/refactor/split_module.py`。
#[cfg(test)]
mod period_test_support; // PERIOD-CLOSE 真库用例的公共夹具（`2033-xx` / `P2-` 前缀）
#[cfg(test)]
pub mod stocktake_tests; // 盘点单用例（纯逻辑 + 真库：盘盈 / 盘亏 / 无差异 / 作废回滚 / 审批流 / 负库存）
#[cfg(test)]
pub mod year_end_test_support; // YEAR-END 真库用例的公共夹具（`2037-xx` / `2038-xx` / `YE-` 前缀）

mod model_base; // 底座：分页归一化 / id 解析 / 通用取数模板
mod model_inventory; // 库存台账只读
mod model_master; // 主数据只读（单位/仓库/库位/物料/供应商/科目/期间/取号规则）
mod model_order; // 采购订单只读（头 + 明细）
mod stockin_base; // 收货入库底座：常量 / 状态机 / 行结构 / 取号
mod stockin_cancel; // 收货入库作废链路
mod stockin_location; // 收货入库库位解析（P2-4：库位字典 ↔ 库位账行）
mod stockin_purchase; // 收货入库↔采购单联动
pub mod stockin_query; // 收货入库只读（`POST_BIZ_TYPE` / `CANCEL_BIZ_TYPE` 是审批词表的登记点，见 doc_kind）
mod stockin_validate; // 收货入库校验（可作废前置）
mod stockin_write; // 收货入库写入（建改删 + 过账）
mod stockout_base; // 出库单底座：常量 / 状态机 / 行结构 / 取号
mod transfer_base; // 调拨单底座：常量 / 6 态状态机 / 行结构 / 取号
mod transfer_query; // 调拨单只读
mod transfer_scan; // 调拨单在途超期扫描
mod transfer_settle; // 调拨单过账结算
mod transfer_write; // 调拨单写入（建改删 + 状态流转）

// ==================== TD-2 拆分出的部件（只经 facade 对外） ====================
// `stockin_handler.rs`（9394 行，含 5990 行测试）按职责拆成 7 个代码部件 + 13 个测试部件，
// 公开 API 由 `stockin_handler` facade 用 `pub use super::<部件>::*;` 重导出，调用方路径不变。
// 拆法见 `docs/refactor-methodology.md`（TD-2 章节）。
mod stockin_handler_cancel; // 作废：提交/撤回作废审批 + 审批后反冲
mod stockin_handler_common; // 底座：错误构造 / 行计划 / 快照与派生总额 / 锁与权限判定
mod stockin_handler_post; // 过账：提交审批 + 审批后过账 + 事务内扣库与成本
mod stockin_handler_purchase; // 采购联动：来源单解析 / 供应商守卫 / 行回填 / 过账前复查
mod stockin_handler_query; // 只读入口：分页 / 详情
mod stockin_handler_status; // 状态流转与其余 HTTP 入口
mod stockin_handler_write; // 写入：建单 / 复制 / 编辑 / 删除

// `transfer_handler.rs`（4676 行，含 3249 行测试）按职责拆成 6 个代码部件 + 7 个测试部件，
// 公开 API 由 `transfer_handler` facade 用 `pub use super::<部件>::*;` 重导出，调用方路径不变。
// 拆法见 `docs/refactor-methodology.md`（TD-2 章节）。
mod transfer_handler_common; // 底座：错误出口（409/404/写库映射）/ 入参解析 / 明细预处理
mod transfer_handler_create; // 建单 / 复制 / 改单（取号 + 明细落库 + 草稿态校验）
mod transfer_handler_delete; // 软删（仅 DRAFT / CANCELLED）
mod transfer_handler_query; // 只读：分页 / 详情 / 涉及序列号 / 在途超期汇总
mod transfer_handler_status; // 六态状态机：审核 / 发出 / 在途 / 收货（含结算）/ 作废 + 审批发起
#[cfg(test)]
mod transfer_handler_test_support; // 测试支撑：把审批实例批到终态（真库用例共用）

/// R-P1-5 的真库单测（联动作废：影响分析 / 执行顺序 / 中途失败回滚 / 无下游直接作废）。
/// 只在 `cargo test` 时编译。
#[cfg(test)]
mod cancel_cascade_tests;

/// 凭证辅助核算（DC-11 · P2-1）：写入口径 + 辅助账查询。只在 `cargo test` 时编译。
#[cfg(test)]
mod aux_tests;

/// 发货单列表批量补齐（缺陷卡 t_45900ea7）：列表与详情逐字段同口径。只在 `cargo test` 时编译。
#[cfg(test)]
mod shipment_page_fill_tests;

/// 夹具卫生（`#[cfg(test)]`）：`biz_status_log` 孤儿行的统一判据与清理。
#[cfg(test)]
pub(crate) mod fixture_hygiene;
/// P-FLOW-3B 的真库单测（应付作废 + 凭证红冲审批）。只在 `cargo test` 时编译。
#[cfg(test)]
mod pflow3b_tests;

/// R-P1-2 的真库单测（业务提醒任务包：幂等 / 收件人 / 真机扫描）。只在 `cargo test` 时编译。
#[cfg(test)]
mod reminder_tests;

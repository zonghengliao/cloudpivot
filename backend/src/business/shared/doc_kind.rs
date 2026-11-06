//! 业务类型词表（**唯一真相**）—— ARCH-3 §5 DC-21 / 卡 P2-5。
//!
//! # 为什么要这个文件
//!
//! 收敛前全系统有 7 套「业务类型」词表，同一个业务对象在不同表里叫不同名字：
//! 一张采购单在 `biz_flow_def.biz_type` 叫 `PO_APPROVE`、在 `biz_status_log.entity_type`
//! 叫 `PURCHASE_ORDER`、在 REST 路径里叫 `purchase-order`、单号前缀又叫 `PO`。
//! 每加一种单据要改 5–7 处字符串常量，漏一处就**静默不联动**
//! （线上表现为「某种单据的通知 / 日志 / 流水缺一块」）。
//!
//! # 盘点结论：不是一根轴，是七根轴
//!
//! 逐列 grep + 生产库 distinct 实测（`docs/architecture/P2-5-业务类型词表盘点.md`）证明
//! 这些列**并不都是**「同一概念的不同写法」，里面有七根互相独立的概念轴。
//! 把它们硬合并成一个字符串会毁掉业务语义（卡面「不改业务流程语义」），
//! 所以本模块的做法是 —— **一个文件、七根轴、一套拼写规则、显式的跨轴映射**：
//!
//! | # | 轴 | 类型 | 回答的问题 | 落库位置 |
//! |---|---|---|---|---|
//! | ① | 业务对象 | [`DocKind`] | 这是哪张单据 / 哪个对象 | `biz_status_log.entity_type`、`biz_doc_attachment.doc_type`、`biz_reminder_log.doc_type` |
//! | ② | 审批动作 | [`FlowBizType`] | 走哪一类审批 | `biz_flow_def.biz_type`、`biz_flow_instance.biz_type` |
//! | ③ | 库存变动动作 | [`TxnBizType`] | 这笔库存动账是什么业务动作 | `biz_inventory_txn.biz_type`、`biz_voucher_rule.biz_type` |
//! | ④ | 流水来源单据 | [`TxnDocType`] | 这笔流水是哪张单据造成的 | `biz_inventory_txn.doc_type` |
//! | ⑤ | 凭证来源 | [`VoucherSourceType`] | 这张凭证从哪来 | `biz_voucher.source_type`、`biz_payable.source_type` |
//! | ⑥ | 序列号来源 | [`SerialSourceType`] | 这个序列号怎么进来的 | `biz_material_serial.source_type` |
//! | ⑦ | 单据子类型 | [`DocSubtype`] / [`TakeType`] | 这张单自身为什么开 | `biz_stock_in.in_type`、`biz_stock_out.out_type`、`biz_stock_take.take_type` |
//!
//! 跨轴关系**只在这里写一次**：[`FlowBizType::doc_kind`]、[`TxnBizType::doc_kind`]、
//! [`TxnDocType::doc_kind`]、[`VoucherSourceType::doc_kind`]，不再是各模块各写一遍 if/else。
//!
//! # 拼写规则（唯一）
//!
//! 落库值一律 **大写下划线**（`PURCHASE_ORDER`），与 `biz_flow_def.biz_type` 对齐
//! （它最接近「业务类型」本义）。唯一例外是 REST 路径段，见 [`DocKind::route`] ——
//! URL 用小写连字符（`purchase-order`）是 REST 惯例，**只由本模块生成**，调用方不许自己拼。
//!
//! # 三处防护
//!
//! 1. **编译期**：调用方只能拿枚举，拿不到裸字符串 —— 新增业务类型不改这里就编译不过；
//! 2. **单测**：[`doc_kind_tests`] 钉住「枚举 ↔ 前端 TS 常量表 ↔ 值域配置」三者逐字一致；
//! 3. **巡检**：`scripts/verify/biz_type_domain.py` 按 [`COLUMN_DOMAINS`] 查库，
//!    任何列出现域外值即 FAIL（含「历史上用过、现在已废弃」的别名，如 `TRANSFER`）。
//!
//! # 不在本文件的「同名不同义」列（登记在案，别误当成业务类型）
//!
//! | 列 | 真实语义 | 域 |
//! |---|---|---|
//! | `biz_flow_cc.source_type` | 抄送人的**来源**（与 `assignee_source` 同域） | `USER` / `ROLE` / `DEPT` / `ADMIN` … |
//! | `sys_import_task.biz_type` / `sys_export_task.biz_type` | **导入导出定义编码**（点分命名空间） | `system.dict_item` … |
//! | `sys_notice.biz_type` / `sys_notice.target_type` | 站内信**分组** / 接收人范围 | `flow` / `task` / `USER` / `ROLE` / `ALL` … |
//! | `operation_logs.target_type` / `audit_logs.target_type` | 审计对象的**类名**（CamelCase） | `User` / `Role` / `Menu` … |

// ==================== ① 业务对象 ====================

/// 业务对象类型（`biz_status_log.entity_type` 与各 `doc_type` 的**唯一**取值域）。
///
/// 落库值 = [`DocKind::as_str`]，全大写下划线。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DocKind {
    /// 采购订单 `biz_purchase_order`。
    PurchaseOrder,
    /// 收货入库单 `biz_stock_in`。
    StockIn,
    /// 出库单 `biz_stock_out`。
    StockOut,
    /// 库存调拨单 `biz_stock_transfer`。
    ///
    /// **本卡收敛点**：`biz_status_log.entity_type` 历史上写 `TRANSFER`
    /// （`state_machine::ENTITY_TRANSFER` 旧值），与本表 / `doc_type` / 审批 `biz_type`
    /// 三处的 `STOCK_TRANSFER` 对不上 —— 报表按 `entity_type` 分组会把调拨拆成两行。
    /// V0219 已把存量改成 `STOCK_TRANSFER`。
    StockTransfer,
    /// 盘点单 `biz_stock_take`。
    StockTake,
    /// 应付单 `biz_payable`。
    Payable,
    /// 付款单 `biz_payment`。
    Payment,
    /// 凭证 `biz_voucher`。
    Voucher,
    /// 合同 `biz_contract`。
    Contract,
    /// 库存 `biz_inventory` —— **不是单据**，是「低库存提醒」的被提醒对象。
    Inventory,
    /// 会计期间 `biz_accounting_period`。
    AccountingPeriod,
    /// 会计年度（年结）—— 没有实体表，`entity_id` 存结转凭证 id。
    AccountingYear,
    /// 状态机边配置 `biz_flow_edge`（配置实体，不是单据）。
    FlowEdgeConfig,
    /// 联动规则配置 `biz_flow_linkage_rule`（配置实体，不是单据）。
    FlowLinkageConfig,
    /// 预算 `biz_budget`（**一条记录 = 一个版本**，ARCH-6 第三章）。
    ///
    /// 与其它单据的区别：预算的「调整」不是改字段，而是**新增版本行**
    /// （`version_no + 1` + `supersedes_id`），旧行只改状态（`ACTIVE → SUPERSEDED`）。
    Budget,
    /// 固定资产卡片 `biz_fixed_asset`（ARCH-6 第五章 · FIN-2）。
    ///
    /// 一台设备 / 一处房产一张卡；**同型号多台请建多张卡**（与 ARCH-6 §5.2.2 的
    /// 「不做卡片数量字段」一致）。
    FixedAsset,
    /// 折旧批次 `biz_depreciation_batch`（一个期间一批，`entity_no` 存期间 `yyyy-MM`）。
    ///
    /// 为什么不是 `biz_depreciation`（明细）：一次计提是**一个整体动作**，
    /// 财务问的是「10 月的折旧提了吗 / 提了多少 / 凭证号多少 / 红冲过吗」——
    /// 这四个问题的答案都在批次行上（ARCH-6 §5.2.4）。
    Depreciation,
    /// 费用报销单 `biz_expense_report`（ARCH-6 第四章 · FIN-3）。
    ///
    /// 与应付的区别（ARCH-6 D1）：债权人是**员工**不是供应商、没有账期、
    /// 有明细（多费用类型 / 多税率）、科目是 `2241.01` 不是 `2202`。
    /// 付款记录 `biz_expense_payment` 不是独立业务对象（它是报销单的从属台账），
    /// 所以本枚举**没有** `ExpensePayment` —— 付款凭证的 `source_type` 走
    /// [`VoucherSourceType::ExpensePayment`]，挂在本对象上。
    ExpenseReport,
    /// 销售订单 `biz_sales_order`（销售链 · SO-04/SO-05）。
    ///
    /// 状态机 7 值（PRD §4.13.3）：`DRAFT`/`SUBMITTED`/`APPROVED`/`PARTIAL_OUT`/
    /// `OUTBOUND`/`CLOSED`/`CANCELLED`，边表在 `state_machine::SALES_ORDER_EDGES`。
    /// 明细 `biz_sales_order_item` 没有状态、也不单独记 `doc_type`（属单头）。
    SaleOrder,
    /// 发货单 `biz_shipment`（销售链 · SO-08/SO-09）。
    ///
    /// 状态机 4 值（ARCH-7 §4.2）：`DRAFT`/`PENDING_POST`/`POSTED`/`CANCELLED`，
    /// 边表在 `state_machine::SHIPMENT_EDGES`。明细 `biz_shipment_item` 没有状态、
    /// 也不单独记 `doc_type`（属单头）。
    Shipment,
    /// 应收单 `biz_receivable`（销售链 · SO-13）。
    ///
    /// 镜像 `Payable`，但状态域不同（ARCH-7 §4.3 + V0290）：`OPEN`/`PARTIAL`/`SETTLED`/
    /// `WRITTEN_OFF`/`CANCELLED`，边表在 `state_machine::RECEIVABLE_EDGES`。没有明细子表，
    /// 状态只在单头流转；坏账核销与出库作废冲销都是终态边（必须给原因）。
    Receivable,
    /// 收款单 `biz_receipt`（销售链 · SO-17）。
    ///
    /// **资金单据**，状态域（ARCH-7 §4.4）：`DRAFT`/`CONFIRMED`/`POSTED`/`CANCELLED`，
    /// 边表在 `state_machine::RECEIPT_EDGES`。核销明细 `biz_receipt_allocation`
    /// 没有状态、也不单独记 `doc_type`（属单头）。
    Receipt,
}

impl DocKind {
    /// 全部业务对象（顺序 = 声明顺序；前端 TS 常量表与巡检按此顺序逐字比对）。
    pub const ALL: [DocKind; 22] = [
        DocKind::PurchaseOrder,
        DocKind::StockIn,
        DocKind::StockOut,
        DocKind::StockTransfer,
        DocKind::StockTake,
        DocKind::Payable,
        DocKind::Payment,
        DocKind::Voucher,
        DocKind::Contract,
        DocKind::Inventory,
        DocKind::AccountingPeriod,
        DocKind::AccountingYear,
        DocKind::FlowEdgeConfig,
        DocKind::FlowLinkageConfig,
        DocKind::Budget,
        DocKind::FixedAsset,
        DocKind::Depreciation,
        DocKind::ExpenseReport,
        DocKind::SaleOrder,
        DocKind::Shipment,
        DocKind::Receivable,
        DocKind::Receipt,
    ];

    /// 落库值（大写下划线）。**唯一拼写**。
    pub const fn as_str(self) -> &'static str {
        match self {
            DocKind::PurchaseOrder => "PURCHASE_ORDER",
            DocKind::StockIn => "STOCK_IN",
            DocKind::StockOut => "STOCK_OUT",
            DocKind::StockTransfer => "STOCK_TRANSFER",
            DocKind::StockTake => "STOCK_TAKE",
            DocKind::Payable => "PAYABLE",
            DocKind::Payment => "PAYMENT",
            DocKind::Voucher => "VOUCHER",
            DocKind::Contract => "CONTRACT",
            DocKind::Inventory => "INVENTORY",
            DocKind::AccountingPeriod => "ACCOUNTING_PERIOD",
            DocKind::AccountingYear => "ACCOUNTING_YEAR",
            DocKind::FlowEdgeConfig => "FLOW_EDGE_CFG",
            DocKind::FlowLinkageConfig => "FLOW_LINKAGE_CFG",
            DocKind::Budget => "BUDGET",
            DocKind::FixedAsset => "FIXED_ASSET",
            DocKind::Depreciation => "DEPRECIATION",
            DocKind::ExpenseReport => "EXPENSE_REPORT",
            DocKind::SaleOrder => "SALE_ORDER",
            DocKind::Shipment => "SHIPMENT",
            DocKind::Receivable => "RECEIVABLE",
            DocKind::Receipt => "RECEIPT",
        }
    }

    /// 中文名（错误文案 / 日志 / 巡检输出共用）。
    pub const fn label(self) -> &'static str {
        match self {
            DocKind::PurchaseOrder => "采购订单",
            DocKind::StockIn => "收货入库单",
            DocKind::StockOut => "出库单",
            DocKind::StockTransfer => "库存调拨单",
            DocKind::StockTake => "盘点单",
            DocKind::Payable => "应付单",
            DocKind::Payment => "付款单",
            DocKind::Voucher => "凭证",
            DocKind::Contract => "合同",
            DocKind::Inventory => "库存",
            DocKind::AccountingPeriod => "会计期间",
            DocKind::AccountingYear => "会计年度",
            DocKind::FlowEdgeConfig => "状态机配置",
            DocKind::FlowLinkageConfig => "联动规则配置",
            DocKind::Budget => "预算",
            DocKind::FixedAsset => "固定资产",
            DocKind::Depreciation => "折旧批次",
            DocKind::ExpenseReport => "费用报销单",
            DocKind::SaleOrder => "销售订单",
            DocKind::Shipment => "发货单",
            DocKind::Receivable => "应收单",
            DocKind::Receipt => "收款单",
        }
    }

    /// 对应的实体表（虚拟对象为 `None`）。
    pub const fn table(self) -> Option<&'static str> {
        Some(match self {
            DocKind::PurchaseOrder => "biz_purchase_order",
            DocKind::StockIn => "biz_stock_in",
            DocKind::StockOut => "biz_stock_out",
            DocKind::StockTransfer => "biz_stock_transfer",
            DocKind::StockTake => "biz_stock_take",
            DocKind::Payable => "biz_payable",
            DocKind::Payment => "biz_payment",
            DocKind::Voucher => "biz_voucher",
            DocKind::Contract => "biz_contract",
            DocKind::Inventory => "biz_inventory",
            DocKind::AccountingPeriod => "biz_accounting_period",
            DocKind::FlowEdgeConfig => "biz_flow_edge",
            DocKind::FlowLinkageConfig => "biz_flow_linkage_rule",
            DocKind::Budget => "biz_budget",
            DocKind::FixedAsset => "biz_fixed_asset",
            DocKind::Depreciation => "biz_depreciation_batch",
            DocKind::ExpenseReport => "biz_expense_report",
            DocKind::SaleOrder => "biz_sales_order",
            DocKind::Shipment => "biz_shipment",
            DocKind::Receivable => "biz_receivable",
            DocKind::Receipt => "biz_receipt",
            // 年结没有实体表：状态记在 `biz_status_log`，`entity_id` 存结转凭证 id
            DocKind::AccountingYear => return None,
        })
    }

    /// 是不是「可作废 / 可审批的业务单据」（配置实体与虚拟对象为 `false`）。
    pub const fn is_document(self) -> bool {
        matches!(
            self,
            DocKind::PurchaseOrder
                | DocKind::StockIn
                | DocKind::StockOut
                | DocKind::StockTransfer
                | DocKind::StockTake
                | DocKind::Payable
                | DocKind::Payment
                | DocKind::Voucher
                | DocKind::Contract
                | DocKind::Budget
                | DocKind::FixedAsset
                | DocKind::Depreciation
                | DocKind::ExpenseReport
                | DocKind::SaleOrder
                | DocKind::Shipment
                | DocKind::Receivable
                | DocKind::Receipt
        )
    }

    /// REST 路径段（**唯一**允许的小写连字符写法）。
    ///
    /// `/{route}/{id}/cancel-impact` 里的 `{route}` 由它生成；不支持的对象返回 `None`。
    pub const fn route(self) -> Option<&'static str> {
        Some(match self {
            DocKind::PurchaseOrder => "purchase-order",
            DocKind::StockIn => "stock-in",
            DocKind::Payable => "payable",
            DocKind::Voucher => "voucher",
            DocKind::StockOut => "stock-out",
            DocKind::StockTransfer => "stock-transfer",
            DocKind::Budget => "budget",
            DocKind::FixedAsset => "fixed-asset",
            DocKind::Depreciation => "depreciation",
            DocKind::ExpenseReport => "expense",
            DocKind::SaleOrder => "sales-order",
            DocKind::Shipment => "shipment",
            DocKind::Receivable => "receivable",
            DocKind::Receipt => "receipt",
            _ => return None,
        })
    }

    /// 路径段的**非 Option** 版本：调用点明知该对象有路径段时用它，
    /// 缺了就是编程错误（`const` 上下文里 panic 会被编译器拒绝，等价于编译期检查）。
    pub const fn route_expect(self) -> &'static str {
        match self.route() {
            Some(route) => route,
            None => panic!("DocKind 没有 REST 路径段（不该在这里取）"),
        }
    }

    /// 字符串 → 业务对象。**严格全等匹配，不做大小写 / 分隔符归一**
    /// （归一就是「新旧都认」，等于把问题永久保留）。
    pub fn parse(raw: &str) -> Option<DocKind> {
        DocKind::ALL.into_iter().find(|item| item.as_str() == raw)
    }

    /// 路径段 → 业务对象（只认 [`DocKind::route`] 的规范写法）。
    pub fn parse_route(raw: &str) -> Option<DocKind> {
        DocKind::ALL
            .into_iter()
            .find(|item| item.route() == Some(raw))
    }
}

/// 能挂附件的对象（与 `ck_biz_doc_attachment_type` 逐字一致）。
///
/// 加成员必须同时改 V0212 的 CHECK，否则写库直接 `check constraint violated`。
pub const ATTACHMENT_DOC_KINDS: [DocKind; 7] = [
    DocKind::Contract,
    DocKind::PurchaseOrder,
    DocKind::StockIn,
    DocKind::Payable,
    DocKind::Payment,
    // FIN-2：资产的发票 / 验收单 / 处置单（ARCH-6 §6.2.4）。
    DocKind::FixedAsset,
    // FIN-3：报销单的发票影像（V0244 把 CHECK 放宽到 7 值）。
    DocKind::ExpenseReport,
];

/// 能记业务提醒的对象（与 `ck_biz_reminder_log_doc_type` 逐字一致）。
///
/// 加成员必须同时改 V0257 的 CHECK，否则写库直接 `check constraint violated`。
pub const REMINDER_DOC_KINDS: [DocKind; 8] = [
    DocKind::Payable,
    DocKind::PurchaseOrder,
    // P1-FIX-10：结账催办的挡路单据（凭证 / 收货单 / 调拨单）也要进台账 ——
    // 4 类定时扫描只覆盖应付 / 采购 / 合同 / 库存，而结账清单里的挡路单据
    // 有一半是这三类（`period_items` 的 DOC_VOUCHER / DOC_STOCK_IN / DOC_STOCK_TRANSFER）。
    DocKind::StockIn,
    DocKind::StockTransfer,
    DocKind::Voucher,
    DocKind::Contract,
    DocKind::Inventory,
    // FIN-3：待付款提醒（V0244 把 CHECK 放宽到 5 值；扫描任务属 P1-5）。
    DocKind::ExpenseReport,
];

// ==================== ② 审批动作 ====================

/// 审批业务类型（`biz_flow_def.biz_type` / `biz_flow_instance.biz_type` 的**唯一**取值域）。
///
/// 与 [`DocKind`] 的区别：这是「**动作**」不是「对象」。
/// 同一张收货单有 `STOCK_IN_POST`（过账）与 `STOCK_IN_CANCEL`（作废）两条审批，
/// 所以审批轴必须比对象轴细 —— 两者的关系由 [`FlowBizType::doc_kind`] 显式给出。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FlowBizType {
    /// 采购订单提交审批（`po_approve`）。
    PoApprove,
    /// 收货过账审批（`stock_in_post`）。
    StockInPost,
    /// 收货作废审批（`stock_in_cancel`）。
    StockInCancel,
    /// 盘点过账审批（`stock_take_post`）。
    StockTakePost,
    /// 库存调拨审批（`stock_transfer`）。
    StockTransfer,
    /// 应付付款审批（`payable_pay`）。
    PayablePay,
    /// 应付作废审批（`payable_cancel`）。
    PayableCancel,
    /// 凭证红冲审批（`voucher_reverse`）。
    VoucherReverse,
    /// 合同审批（`contract_approve`）。
    ContractApprove,
    /// 任务验收（`task_accept`）。
    TaskAccept,
    /// 状态机配置变更审批（`flow_edge_config`）。
    FlowEdgeConfig,
    /// 费用类（通用报销，无 ERP 桥接）。
    Expense,
    /// 客户端自建流程的兜底类型（**不在自检范围**）。
    Other,
    /// 预算编制 / 调整审批（`budget_approve`，ARCH-6 §3.4.2 / §3.4.3）。
    ///
    /// 编制与调整**共用一个流程**，靠 `form.change_type` 条件分流：
    /// `ORIGINAL` 走「部门主管 → 财务（≥50 万加总经理会签）」，
    /// `ADJUST` 走「财务前置 → 总经理（≥50 万加财务总监会签）」。
    BudgetApprove,
    /// 资产处置审批（`asset_disposal`，ARCH-6 §5.6.3）。
    ///
    /// 处置资产 = 把公司资产变成现金或废品，是最容易发生舞弊的环节
    /// （低价卖给关联方、假装报废后私用），所以**不分金额档、第一站就是总经理**
    /// （频次低，多一道签的成本可以接受）。
    AssetDisposal,
}

impl FlowBizType {
    /// 全部审批业务类型（顺序 = 声明顺序）。
    pub const ALL: [FlowBizType; 15] = [
        FlowBizType::PoApprove,
        FlowBizType::StockInPost,
        FlowBizType::StockInCancel,
        FlowBizType::StockTakePost,
        FlowBizType::StockTransfer,
        FlowBizType::PayablePay,
        FlowBizType::PayableCancel,
        FlowBizType::VoucherReverse,
        FlowBizType::ContractApprove,
        FlowBizType::TaskAccept,
        FlowBizType::FlowEdgeConfig,
        FlowBizType::Expense,
        FlowBizType::Other,
        FlowBizType::BudgetApprove,
        FlowBizType::AssetDisposal,
    ];

    /// 需要「发布状态自检」的业务类型（`GET /business/erp/approval-flow/health` 的清单）。
    ///
    /// 与 `V0202__erp_approval_flow_health.up.sql` 的自检块逐字一致（8 个）。
    /// `CONTRACT_APPROVE` / `STOCK_TAKE_POST` / `FLOW_EDGE_CFG` 也有预置流程，
    /// 但历史卡只把 8 个纳入了健康检查 —— 扩清单要同步改那条迁移。
    pub const HEALTH_CHECKED: [FlowBizType; 8] = [
        FlowBizType::PoApprove,
        FlowBizType::StockInPost,
        FlowBizType::StockInCancel,
        FlowBizType::PayablePay,
        FlowBizType::PayableCancel,
        FlowBizType::VoucherReverse,
        FlowBizType::StockTransfer,
        FlowBizType::TaskAccept,
    ];

    /// 落库值（大写下划线）。**唯一拼写**。
    pub const fn as_str(self) -> &'static str {
        match self {
            FlowBizType::PoApprove => "PO_APPROVE",
            FlowBizType::StockInPost => "STOCK_IN_POST",
            FlowBizType::StockInCancel => "STOCK_IN_CANCEL",
            FlowBizType::StockTakePost => "STOCK_TAKE_POST",
            FlowBizType::StockTransfer => "STOCK_TRANSFER",
            FlowBizType::PayablePay => "PAYABLE_PAY",
            FlowBizType::PayableCancel => "PAYABLE_CANCEL",
            FlowBizType::VoucherReverse => "VOUCHER_REVERSE",
            FlowBizType::ContractApprove => "CONTRACT_APPROVE",
            FlowBizType::TaskAccept => "TASK_ACCEPT",
            FlowBizType::FlowEdgeConfig => "FLOW_EDGE_CFG",
            FlowBizType::Expense => "EXPENSE",
            FlowBizType::Other => "OTHER",
            FlowBizType::BudgetApprove => "BUDGET_APPROVE",
            FlowBizType::AssetDisposal => "ASSET_DISPOSAL",
        }
    }

    /// 默认流程编码（`biz_flow_def.flow_code`，小写蛇形）。
    pub const fn flow_code(self) -> &'static str {
        match self {
            FlowBizType::PoApprove => "po_approve",
            FlowBizType::StockInPost => "stock_in_post",
            FlowBizType::StockInCancel => "stock_in_cancel",
            FlowBizType::StockTakePost => "stock_take_post",
            FlowBizType::StockTransfer => "stock_transfer",
            FlowBizType::PayablePay => "payable_pay",
            FlowBizType::PayableCancel => "payable_cancel",
            FlowBizType::VoucherReverse => "voucher_reverse",
            FlowBizType::ContractApprove => "contract_approve",
            FlowBizType::TaskAccept => "task_accept",
            FlowBizType::FlowEdgeConfig => "flow_edge_config",
            FlowBizType::Expense => "expense",
            FlowBizType::Other => "other",
            FlowBizType::BudgetApprove => "budget_approve",
            FlowBizType::AssetDisposal => "asset_disposal",
        }
    }

    /// 中文名。
    pub const fn label(self) -> &'static str {
        match self {
            FlowBizType::PoApprove => "采购订单审批",
            FlowBizType::StockInPost => "收货过账审批",
            FlowBizType::StockInCancel => "收货作废审批",
            FlowBizType::StockTakePost => "盘点过账审批",
            FlowBizType::StockTransfer => "库存调拨审批",
            FlowBizType::PayablePay => "应付付款审批",
            FlowBizType::PayableCancel => "应付作废审批",
            FlowBizType::VoucherReverse => "凭证红冲审批",
            FlowBizType::ContractApprove => "合同审批",
            FlowBizType::TaskAccept => "任务验收",
            FlowBizType::FlowEdgeConfig => "状态机配置变更审批",
            FlowBizType::Expense => "费用审批",
            FlowBizType::Other => "其他审批",
            FlowBizType::BudgetApprove => "预算审批",
            FlowBizType::AssetDisposal => "资产处置审批",
        }
    }

    /// 这条审批挂在哪个业务对象上（审批轴 → 对象轴的**唯一**映射）。
    ///
    /// `None` = 不是 ERP 单据（任务验收 / 费用 / 自定义流程），不要硬塞一个对象进来。
    pub const fn doc_kind(self) -> Option<DocKind> {
        Some(match self {
            FlowBizType::PoApprove => DocKind::PurchaseOrder,
            FlowBizType::StockInPost | FlowBizType::StockInCancel => DocKind::StockIn,
            FlowBizType::StockTakePost => DocKind::StockTake,
            FlowBizType::StockTransfer => DocKind::StockTransfer,
            FlowBizType::PayablePay | FlowBizType::PayableCancel => DocKind::Payable,
            FlowBizType::VoucherReverse => DocKind::Voucher,
            FlowBizType::ContractApprove => DocKind::Contract,
            FlowBizType::FlowEdgeConfig => DocKind::FlowEdgeConfig,
            FlowBizType::BudgetApprove => DocKind::Budget,
            FlowBizType::AssetDisposal => DocKind::FixedAsset,
            // FIN-3（ARCH-6 §6.2.2）：`Expense` 由 `None` 改成 `Some(...)` —— 一处**行为变更**，
            // `doc_kind_tests` 的期望同步改；`HEALTH_CHECKED` 未扩（它绑着已应用的 V0202
            // 自检块，改注释都会 checksum 失败，扩清单必须新开迁移重写自检块，超出 FIN-3 范围）。
            FlowBizType::Expense => DocKind::ExpenseReport,
            FlowBizType::TaskAccept | FlowBizType::Other => return None,
        })
    }

    /// 字符串 → 审批业务类型（严格全等）。
    pub fn parse(raw: &str) -> Option<FlowBizType> {
        FlowBizType::ALL
            .into_iter()
            .find(|item| item.as_str() == raw)
    }
}

// ==================== ③ 库存变动动作 ====================

/// 库存流水业务动作（`biz_inventory_txn.biz_type` 的**唯一**取值域）。
///
/// 一次调拨产生 `TRANSFER_OUT` + `TRANSFER_IN` 两条流水，一次收货产生
/// `PURCHASE_IN` / `PURCHASE_IN_CANCEL` —— 动账轴天生比对象轴细。
///
/// # 盘点的两条来源（别混：`ADJUST_*` 是调整动作，`STOCK_TAKE_*` 是盘点单）
///
/// * `ADJUST_IN` / `ADJUST_OUT`：泛化的「调整」动作（历史值，现行代码 /
///   `scripts/` 里已无写入点，保留在域内以容纳存量）；
/// * `STOCK_TAKE_GAIN` / `STOCK_TAKE_LOSS` / `STOCK_TAKE_CANCEL`：**盘点单过账**写的三个值
///   （`stocktake_model::BIZ_TYPE_GAIN/LOSS/CANCEL`，`doc_type='STOCK_TAKE'`）。
///   三态是盘点业务的事实（盘盈 / 盘亏 / 作废回补方向与正向相反），
///   与 `ADJUST_*` 不是同义词 —— 收敛成两个值会让「这张流水是哪张盘点单造成的」
///   与「方向」两件事挤在一个字段里。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TxnBizType {
    /// 采购入库（收货过账）。
    PurchaseIn,
    /// 采购入库红冲（收货作废）。
    PurchaseInCancel,
    /// 采购退货出库。
    PurchaseReturn,
    /// 销售出库。
    SaleOut,
    /// 生产入库。
    ProductionIn,
    /// 盘盈（调整入）。
    AdjustIn,
    /// 盘亏（调整出）。
    AdjustOut,
    /// 调拨出库（发运，进在途仓）。
    TransferOut,
    /// 调拨入库（签收，出在途仓）。
    TransferIn,
    /// 领用出库。
    IssueOut,
    /// 报废出库。
    ScrapOut,
    /// 盘亏出库（走 `biz_stock_out` 的 `LOSS`）。
    LossOut,
    /// 其他出库。
    OtherOut,
    /// 出库单作废回滚。
    StockOutCancel,
    /// 生产入库红冲。
    ProduceCancel,
    /// 一物一码：序列号入库。
    SerialIn,
    /// 一物一码：序列号出库。
    SerialOut,
    /// 一物一码：序列号报废。
    SerialScrap,
    /// 期初余额导入。
    Opening,
    /// 盘点盘盈（盘点单过账，`txn_type='IN'`）。
    StockTakeGain,
    /// 盘点盘亏（盘点单过账，`txn_type='OUT'`）。
    StockTakeLoss,
    /// 盘点作废回补（已过账盘点单作废，方向与正向相反）。
    StockTakeCancel,
}

impl TxnBizType {
    /// 全部动账类型（顺序 = 声明顺序）。
    pub const ALL: [TxnBizType; 22] = [
        TxnBizType::PurchaseIn,
        TxnBizType::PurchaseInCancel,
        TxnBizType::PurchaseReturn,
        TxnBizType::SaleOut,
        TxnBizType::ProductionIn,
        TxnBizType::AdjustIn,
        TxnBizType::AdjustOut,
        TxnBizType::TransferOut,
        TxnBizType::TransferIn,
        TxnBizType::IssueOut,
        TxnBizType::ScrapOut,
        TxnBizType::LossOut,
        TxnBizType::OtherOut,
        TxnBizType::StockOutCancel,
        TxnBizType::ProduceCancel,
        TxnBizType::SerialIn,
        TxnBizType::SerialOut,
        TxnBizType::SerialScrap,
        TxnBizType::Opening,
        TxnBizType::StockTakeGain,
        TxnBizType::StockTakeLoss,
        TxnBizType::StockTakeCancel,
    ];

    /// 落库值（大写下划线）。**唯一拼写**。
    pub const fn as_str(self) -> &'static str {
        match self {
            TxnBizType::PurchaseIn => "PURCHASE_IN",
            TxnBizType::PurchaseInCancel => "PURCHASE_IN_CANCEL",
            TxnBizType::PurchaseReturn => "PURCHASE_RETURN",
            TxnBizType::SaleOut => "SALE_OUT",
            TxnBizType::ProductionIn => "PRODUCTION_IN",
            TxnBizType::AdjustIn => "ADJUST_IN",
            TxnBizType::AdjustOut => "ADJUST_OUT",
            TxnBizType::TransferOut => "TRANSFER_OUT",
            TxnBizType::TransferIn => "TRANSFER_IN",
            TxnBizType::IssueOut => "ISSUE_OUT",
            TxnBizType::ScrapOut => "SCRAP_OUT",
            TxnBizType::LossOut => "LOSS_OUT",
            TxnBizType::OtherOut => "OTHER_OUT",
            TxnBizType::StockOutCancel => "STOCK_OUT_CANCEL",
            TxnBizType::ProduceCancel => "PRODUCE_CANCEL",
            TxnBizType::SerialIn => "SERIAL_IN",
            TxnBizType::SerialOut => "SERIAL_OUT",
            TxnBizType::SerialScrap => "SERIAL_SCRAP",
            TxnBizType::Opening => "OPENING",
            TxnBizType::StockTakeGain => "STOCK_TAKE_GAIN",
            TxnBizType::StockTakeLoss => "STOCK_TAKE_LOSS",
            TxnBizType::StockTakeCancel => "STOCK_TAKE_CANCEL",
        }
    }

    /// 中文名。
    pub const fn label(self) -> &'static str {
        match self {
            TxnBizType::PurchaseIn => "采购入库",
            TxnBizType::PurchaseInCancel => "采购入库红冲",
            TxnBizType::PurchaseReturn => "采购退货",
            TxnBizType::SaleOut => "销售出库",
            TxnBizType::ProductionIn => "生产入库",
            TxnBizType::AdjustIn => "盘盈入库",
            TxnBizType::AdjustOut => "盘亏出库",
            TxnBizType::TransferOut => "调拨出库",
            TxnBizType::TransferIn => "调拨入库",
            TxnBizType::IssueOut => "领用出库",
            TxnBizType::ScrapOut => "报废出库",
            TxnBizType::LossOut => "盘亏出库",
            TxnBizType::OtherOut => "其他出库",
            TxnBizType::StockOutCancel => "出库作废",
            TxnBizType::ProduceCancel => "生产入库红冲",
            TxnBizType::SerialIn => "序列号入库",
            TxnBizType::SerialOut => "序列号出库",
            TxnBizType::SerialScrap => "序列号报废",
            TxnBizType::Opening => "期初导入",
            TxnBizType::StockTakeGain => "盘点盘盈",
            TxnBizType::StockTakeLoss => "盘点盘亏",
            TxnBizType::StockTakeCancel => "盘点作废回补",
        }
    }

    /// 这条流水是哪张单据造成的（动账轴 → 对象轴的**唯一**映射）。
    pub const fn doc_kind(self) -> DocKind {
        match self {
            TxnBizType::PurchaseIn
            | TxnBizType::PurchaseInCancel
            | TxnBizType::PurchaseReturn
            | TxnBizType::ProductionIn
            | TxnBizType::ProduceCancel
            | TxnBizType::SerialIn
            | TxnBizType::SerialOut
            | TxnBizType::SerialScrap => DocKind::StockIn,
            TxnBizType::SaleOut
            | TxnBizType::IssueOut
            | TxnBizType::ScrapOut
            | TxnBizType::LossOut
            | TxnBizType::OtherOut
            | TxnBizType::StockOutCancel => DocKind::StockOut,
            TxnBizType::AdjustIn | TxnBizType::AdjustOut => DocKind::StockTake,
            TxnBizType::StockTakeGain | TxnBizType::StockTakeLoss | TxnBizType::StockTakeCancel => {
                DocKind::StockTake
            }
            TxnBizType::TransferOut | TxnBizType::TransferIn => DocKind::StockTransfer,
            TxnBizType::Opening => DocKind::Inventory,
        }
    }

    /// 字符串 → 动账类型（严格全等）。
    pub fn parse(raw: &str) -> Option<TxnBizType> {
        TxnBizType::ALL
            .into_iter()
            .find(|item| item.as_str() == raw)
    }
}

// ==================== ④ 流水来源单据 ====================

/// 库存流水「来源单据类型」（`biz_inventory_txn.doc_type` 的**唯一**取值域）。
///
/// 比 [`DocKind`] 多两个**子表级**来源（一物一码的批次 / 单件），少若干个不发流水的对象，
/// 所以单独一根轴。`ADJUST` 无代码写入（来自 `scripts/datafix/p14` 的历史修正），
/// 保留在域内并在巡检里登记为「存量遗留」，新代码不许再写。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TxnDocType {
    /// 收货入库单。
    StockIn,
    /// 出库单。
    StockOut,
    /// 调拨单。
    StockTransfer,
    /// 盘点单（`stocktake_model::DOC_TYPE`）。
    StockTake,
    /// 一物一码批次（`SERIAL_BATCH`）。
    SerialBatch,
    /// 单个序列号记录（`MATERIAL_SERIAL`）。
    MaterialSerial,
    /// 期初导入。
    Opening,
    /// 库存手工调整（**存量遗留**，无代码写入）。
    Adjust,
}

impl TxnDocType {
    /// 全部来源类型（顺序 = 声明顺序）。
    pub const ALL: [TxnDocType; 8] = [
        TxnDocType::StockIn,
        TxnDocType::StockOut,
        TxnDocType::StockTransfer,
        TxnDocType::StockTake,
        TxnDocType::SerialBatch,
        TxnDocType::MaterialSerial,
        TxnDocType::Opening,
        TxnDocType::Adjust,
    ];

    /// 落库值（大写下划线）。**唯一拼写**。
    pub const fn as_str(self) -> &'static str {
        match self {
            TxnDocType::StockIn => "STOCK_IN",
            TxnDocType::StockOut => "STOCK_OUT",
            TxnDocType::StockTransfer => "STOCK_TRANSFER",
            TxnDocType::StockTake => "STOCK_TAKE",
            TxnDocType::SerialBatch => "SERIAL_BATCH",
            TxnDocType::MaterialSerial => "MATERIAL_SERIAL",
            TxnDocType::Opening => "OPENING",
            TxnDocType::Adjust => "ADJUST",
        }
    }

    /// 中文名。
    pub const fn label(self) -> &'static str {
        match self {
            TxnDocType::StockIn => "收货入库单",
            TxnDocType::StockOut => "出库单",
            TxnDocType::StockTransfer => "库存调拨单",
            TxnDocType::StockTake => "盘点单",
            TxnDocType::SerialBatch => "序列号批次",
            TxnDocType::MaterialSerial => "序列号",
            TxnDocType::Opening => "期初导入",
            TxnDocType::Adjust => "库存调整",
        }
    }

    /// 来源 → 业务对象（唯一映射）。
    pub const fn doc_kind(self) -> DocKind {
        match self {
            TxnDocType::StockIn => DocKind::StockIn,
            TxnDocType::StockOut => DocKind::StockOut,
            TxnDocType::StockTransfer => DocKind::StockTransfer,
            TxnDocType::StockTake => DocKind::StockTake,
            TxnDocType::SerialBatch | TxnDocType::MaterialSerial => DocKind::Inventory,
            TxnDocType::Opening | TxnDocType::Adjust => DocKind::Inventory,
        }
    }

    /// 字符串 → 来源类型（严格全等）。
    pub fn parse(raw: &str) -> Option<TxnDocType> {
        TxnDocType::ALL
            .into_iter()
            .find(|item| item.as_str() == raw)
    }
}

// ==================== ⑤ 凭证 / 应付来源 ====================

/// 凭证来源（`biz_voucher.source_type` 的**唯一**取值域）。
///
/// `biz_payable.source_type` 共用本枚举，但实际只用 [`VoucherSourceType::StockIn`]
/// （应付单目前只由收货过账生成）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VoucherSourceType {
    /// 收货过账生成（自动）。
    StockIn,
    /// 出库过账生成（自动）。
    StockOut,
    /// 应付付款生成（自动）。
    Payment,
    /// 盘点过账生成（自动）。
    StockTake,
    /// 序列号批次（自动）。
    SerialBatch,
    /// 序列号出库（自动）。
    SerialOut,
    /// 序列号报废（自动）。
    SerialScrap,
    /// 期初余额录入生成。
    Opening,
    /// 年结结转生成。
    YearEnd,
    /// 手工新增。
    Manual,
    /// 固定资产购入（建卡时生成，ARCH-6 §5.3.3 / §6.4）。
    FixedAssetAcq,
    /// 固定资产处置（出售 / 报废 / 盘亏，ARCH-6 §5.6.4）。
    FixedAssetDisposal,
    /// 折旧计提（月度批次过账，ARCH-6 §5.5.3）。
    Depreciation,
    /// 费用报销入账（审批通过时生成，ARCH-6 §4.5.1 ①）。
    ExpenseReport,
    /// 费用报销付款（每笔付款一张，ARCH-6 §4.5.1 ②）。
    ExpensePayment,
    /// 收款单过账（借 1002 银行存款 / 贷 1122 应收账款，销售链 · SO-17）。
    Receipt,
}

impl VoucherSourceType {
    /// 全部来源（顺序 = 声明顺序）。
    pub const ALL: [VoucherSourceType; 16] = [
        VoucherSourceType::StockIn,
        VoucherSourceType::StockOut,
        VoucherSourceType::Payment,
        VoucherSourceType::StockTake,
        VoucherSourceType::SerialBatch,
        VoucherSourceType::SerialOut,
        VoucherSourceType::SerialScrap,
        VoucherSourceType::Opening,
        VoucherSourceType::YearEnd,
        VoucherSourceType::Manual,
        VoucherSourceType::FixedAssetAcq,
        VoucherSourceType::FixedAssetDisposal,
        VoucherSourceType::Depreciation,
        VoucherSourceType::ExpenseReport,
        VoucherSourceType::ExpensePayment,
        VoucherSourceType::Receipt,
    ];

    /// 落库值（大写下划线）。**唯一拼写**。
    pub const fn as_str(self) -> &'static str {
        match self {
            VoucherSourceType::StockIn => "STOCK_IN",
            VoucherSourceType::StockOut => "STOCK_OUT",
            VoucherSourceType::Payment => "PAYMENT",
            VoucherSourceType::StockTake => "STOCK_TAKE",
            VoucherSourceType::SerialBatch => "SERIAL_BATCH",
            VoucherSourceType::SerialOut => "SERIAL_OUT",
            VoucherSourceType::SerialScrap => "SERIAL_SCRAP",
            VoucherSourceType::Opening => "OPENING",
            VoucherSourceType::YearEnd => "YEAR_END",
            VoucherSourceType::Manual => "MANUAL",
            VoucherSourceType::FixedAssetAcq => "FIXED_ASSET_ACQ",
            VoucherSourceType::FixedAssetDisposal => "FIXED_ASSET_DISPOSAL",
            VoucherSourceType::Depreciation => "DEPRECIATION",
            VoucherSourceType::ExpenseReport => "EXPENSE_REPORT",
            VoucherSourceType::ExpensePayment => "EXPENSE_PAYMENT",
            VoucherSourceType::Receipt => "RECEIPT",
        }
    }

    /// 中文名。
    pub const fn label(self) -> &'static str {
        match self {
            VoucherSourceType::StockIn => "收货过账",
            VoucherSourceType::StockOut => "出库过账",
            VoucherSourceType::Payment => "应付付款",
            VoucherSourceType::StockTake => "盘点过账",
            VoucherSourceType::SerialBatch => "序列号批次",
            VoucherSourceType::SerialOut => "序列号出库",
            VoucherSourceType::SerialScrap => "序列号报废",
            VoucherSourceType::Opening => "期初录入",
            VoucherSourceType::YearEnd => "年结结转",
            VoucherSourceType::Manual => "手工录入",
            VoucherSourceType::FixedAssetAcq => "固定资产购入",
            VoucherSourceType::FixedAssetDisposal => "固定资产处置",
            VoucherSourceType::Depreciation => "折旧计提",
            VoucherSourceType::ExpenseReport => "费用报销入账",
            VoucherSourceType::ExpensePayment => "费用报销付款",
            VoucherSourceType::Receipt => "收款过账",
        }
    }

    /// 来源 → 业务对象（唯一映射；`Manual` / `YearEnd` / `Opening` 不是某个业务单据）。
    pub const fn doc_kind(self) -> Option<DocKind> {
        Some(match self {
            VoucherSourceType::StockIn => DocKind::StockIn,
            VoucherSourceType::StockOut => DocKind::StockOut,
            VoucherSourceType::Payment => DocKind::Payment,
            VoucherSourceType::StockTake => DocKind::StockTake,
            VoucherSourceType::SerialBatch | VoucherSourceType::SerialOut => DocKind::Inventory,
            VoucherSourceType::SerialScrap => DocKind::Inventory,
            VoucherSourceType::Opening => DocKind::AccountingPeriod,
            VoucherSourceType::YearEnd => DocKind::AccountingYear,
            VoucherSourceType::Manual => return None,
            VoucherSourceType::FixedAssetAcq | VoucherSourceType::FixedAssetDisposal => {
                DocKind::FixedAsset
            }
            VoucherSourceType::Depreciation => DocKind::Depreciation,
            // 入账与付款两张凭证都挂在报销单上（付款记录不是独立业务对象）
            VoucherSourceType::ExpenseReport | VoucherSourceType::ExpensePayment => {
                DocKind::ExpenseReport
            }
            VoucherSourceType::Receipt => DocKind::Receipt,
        })
    }

    /// 字符串 → 来源（严格全等）。
    pub fn parse(raw: &str) -> Option<VoucherSourceType> {
        VoucherSourceType::ALL
            .into_iter()
            .find(|item| item.as_str() == raw)
    }
}

// ==================== ⑥ 序列号来源 ====================

/// 序列号来源（`biz_material_serial.source_type` 的**唯一**取值域）。
///
/// 说的是「这个序列号是怎么进来的」，与收货单的 `in_type` 同语义（`PURCHASE` / `RETURN`），
/// 但多了 `TRANSFER` / `PRODUCE` —— 单独一根轴。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SerialSourceType {
    /// 采购入库。
    Purchase,
    /// 调拨入库。
    Transfer,
    /// 生产入库。
    Produce,
    /// 退货入库。
    Return,
}

impl SerialSourceType {
    /// 全部来源（顺序 = 声明顺序，与 `serial_model::SOURCE_TYPES` 一致）。
    pub const ALL: [SerialSourceType; 4] = [
        SerialSourceType::Purchase,
        SerialSourceType::Transfer,
        SerialSourceType::Produce,
        SerialSourceType::Return,
    ];

    /// 落库值（大写下划线）。**唯一拼写**。
    pub const fn as_str(self) -> &'static str {
        match self {
            SerialSourceType::Purchase => "PURCHASE",
            SerialSourceType::Transfer => "TRANSFER",
            SerialSourceType::Produce => "PRODUCE",
            SerialSourceType::Return => "RETURN",
        }
    }

    /// 中文名。
    pub const fn label(self) -> &'static str {
        match self {
            SerialSourceType::Purchase => "采购入库",
            SerialSourceType::Transfer => "调拨入库",
            SerialSourceType::Produce => "生产入库",
            SerialSourceType::Return => "退货入库",
        }
    }

    /// `source_id` 指向哪张单据表（**唯一映射**，`None` = 这一来源没有实体单据表）。
    ///
    /// 这是「序列号来源」这根轴的**第二半**：`source_type` 只说「怎么进来的」，
    /// 要回跳必须知道 id 落在哪张表。三种有单据的来源与一种没有单据的来源：
    ///
    /// | 来源 | 目标表 | 依据 |
    /// |---|---|---|
    /// | `PURCHASE` | `biz_stock_in` | 收货过账（`in_type=PURCHASE`）生成，`source_id` = 收货单 id |
    /// | `RETURN`   | `biz_stock_in` | 退货入库走**同一张**收货单表（`in_type=RETURN`） |
    /// | `TRANSFER` | `biz_stock_transfer` | 调拨入库单；**注意**：调拨只搬已有序列号，不改它们的来源 |
    /// | `PRODUCE`  | `None` | 生产模块尚未落地，没有「生产入库单」这张表 ⇒ 不记 id |
    ///
    /// 与巡检登记表 `di.v_poly_ref_map`（`ref_domain='SERIAL_SOURCE'`）逐行一致，
    /// 由 `doc_kind_tests::serial_source_map_matches_snapshot_registry` 机械核对 ——
    /// 改这里不改 SQL（或反过来）测试就红。
    pub const fn target_table(self) -> Option<&'static str> {
        Some(match self {
            SerialSourceType::Purchase => "biz_stock_in",
            SerialSourceType::Return => "biz_stock_in",
            SerialSourceType::Transfer => "biz_stock_transfer",
            SerialSourceType::Produce => return None,
        })
    }

    /// 字符串 → 来源（严格全等）。
    pub fn parse(raw: &str) -> Option<SerialSourceType> {
        SerialSourceType::ALL
            .into_iter()
            .find(|item| item.as_str() == raw)
    }
}

// ==================== ⑦ 单据子类型 ====================

/// 单据子类型（`biz_stock_in.in_type` / `biz_stock_out.out_type`）。
///
/// 语义是「这张单自身为什么而开」，与 [`DocKind`]（是哪张单据）正交：
/// `(DocKind::StockIn, DocSubtype::Purchase)` = 采购入库单。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DocSubtype {
    /// 采购入库（`in_type = 'PURCHASE'`，必有供应商）。
    Purchase,
    /// 退货入库（`in_type = 'RETURN'`）。
    Return,
    /// 其他入库（`in_type = 'OTHER'`）。
    OtherIn,
    /// 领用出库（`out_type = 'ISSUE'`，借 6401）。
    Issue,
    /// 报废出库（`out_type = 'SCRAP'`，借 1901）。
    Scrap,
    /// 盘亏出库（`out_type = 'LOSS'`，借 1901）。
    Loss,
    /// 其他出库（`out_type = 'OTHER'`）。
    OtherOut,
}

impl DocSubtype {
    /// 收货单 `in_type` 的全部取值（顺序与 `ck_biz_si_type` 一致）。
    pub const IN_TYPES: [DocSubtype; 3] = [
        DocSubtype::Purchase,
        DocSubtype::Return,
        DocSubtype::OtherIn,
    ];

    /// 出库单 `out_type` 的全部取值（顺序与 `ck_biz_so_type` 一致）。
    pub const OUT_TYPES: [DocSubtype; 4] = [
        DocSubtype::Issue,
        DocSubtype::Scrap,
        DocSubtype::Loss,
        DocSubtype::OtherOut,
    ];

    /// 落库值（大写下划线）。**唯一拼写**。
    pub const fn as_str(self) -> &'static str {
        match self {
            DocSubtype::Purchase => "PURCHASE",
            DocSubtype::Return => "RETURN",
            DocSubtype::OtherIn | DocSubtype::OtherOut => "OTHER",
            DocSubtype::Issue => "ISSUE",
            DocSubtype::Scrap => "SCRAP",
            DocSubtype::Loss => "LOSS",
        }
    }

    /// 中文名。
    pub const fn label(self) -> &'static str {
        match self {
            DocSubtype::Purchase => "采购入库",
            DocSubtype::Return => "退货入库",
            DocSubtype::OtherIn => "其他入库",
            DocSubtype::OtherOut => "其他出库",
            DocSubtype::Issue => "领用出库",
            DocSubtype::Scrap => "报废出库",
            DocSubtype::Loss => "盘亏出库",
        }
    }

    /// 字符串 → 子类型（严格全等；`OTHER` 归入库侧，出库侧由列名区分）。
    pub fn parse(raw: &str) -> Option<DocSubtype> {
        DocSubtype::ALL
            .into_iter()
            .find(|item| item.as_str() == raw)
    }

    /// 全部子类型（`OTHER` 两轴共用同一个字面量，故只出现一次）。
    pub const ALL: [DocSubtype; 6] = [
        DocSubtype::Purchase,
        DocSubtype::Return,
        DocSubtype::OtherIn,
        DocSubtype::Issue,
        DocSubtype::Scrap,
        DocSubtype::Loss,
    ];
}

/// 盘点方式（`biz_stock_take.take_type`，与 `ck_biz_st_type` 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TakeType {
    /// 全盘（建单时把该仓全部库存桶快照成明细）。
    Full,
    /// 抽盘（只盘指定物料）。
    Partial,
}

impl TakeType {
    /// 全部取值（顺序 = 声明顺序）。
    pub const ALL: [TakeType; 2] = [TakeType::Full, TakeType::Partial];

    /// 落库值（大写下划线）。**唯一拼写**。
    pub const fn as_str(self) -> &'static str {
        match self {
            TakeType::Full => "FULL",
            TakeType::Partial => "PARTIAL",
        }
    }

    /// 中文名。
    pub const fn label(self) -> &'static str {
        match self {
            TakeType::Full => "全盘",
            TakeType::Partial => "抽盘",
        }
    }

    /// 字符串 → 盘点方式（严格全等）。
    pub fn parse(raw: &str) -> Option<TakeType> {
        TakeType::ALL.into_iter().find(|item| item.as_str() == raw)
    }
}

// ==================== 值域配置（巡检唯一清单） ====================

/// 一列的合法值域（巡检脚本与单测共用）。
#[derive(Debug, Clone, Copy)]
pub struct ColumnDomain {
    /// 表名。
    pub table: &'static str,
    /// 列名。
    pub column: &'static str,
    /// 合法值（逐字、按枚举声明顺序）。
    pub values: &'static [&'static str],
    /// 这根轴的口径（巡检输出里展示，便于一眼看出「这条红是哪根轴的」）。
    pub axis: &'static str,
}

const DOC_KIND_VALUES: [&str; 22] = [
    "PURCHASE_ORDER",
    "STOCK_IN",
    "STOCK_OUT",
    "STOCK_TRANSFER",
    "STOCK_TAKE",
    "PAYABLE",
    "PAYMENT",
    "VOUCHER",
    "CONTRACT",
    "INVENTORY",
    "ACCOUNTING_PERIOD",
    "ACCOUNTING_YEAR",
    "FLOW_EDGE_CFG",
    "FLOW_LINKAGE_CFG",
    "BUDGET",
    "FIXED_ASSET",
    "DEPRECIATION",
    "EXPENSE_REPORT",
    "SALE_ORDER",
    "SHIPMENT",
    "RECEIVABLE",
    "RECEIPT",
];

const FLOW_BIZ_TYPE_VALUES: [&str; 15] = [
    "PO_APPROVE",
    "STOCK_IN_POST",
    "STOCK_IN_CANCEL",
    "STOCK_TAKE_POST",
    "STOCK_TRANSFER",
    "PAYABLE_PAY",
    "PAYABLE_CANCEL",
    "VOUCHER_REVERSE",
    "CONTRACT_APPROVE",
    "TASK_ACCEPT",
    "FLOW_EDGE_CFG",
    "EXPENSE",
    "OTHER",
    "BUDGET_APPROVE",
    "ASSET_DISPOSAL",
];

const TXN_BIZ_TYPE_VALUES: [&str; 22] = [
    "PURCHASE_IN",
    "PURCHASE_IN_CANCEL",
    "PURCHASE_RETURN",
    "SALE_OUT",
    "PRODUCTION_IN",
    "ADJUST_IN",
    "ADJUST_OUT",
    "TRANSFER_OUT",
    "TRANSFER_IN",
    "ISSUE_OUT",
    "SCRAP_OUT",
    "LOSS_OUT",
    "OTHER_OUT",
    "STOCK_OUT_CANCEL",
    "PRODUCE_CANCEL",
    "SERIAL_IN",
    "SERIAL_OUT",
    "SERIAL_SCRAP",
    "OPENING",
    "STOCK_TAKE_GAIN",
    "STOCK_TAKE_LOSS",
    "STOCK_TAKE_CANCEL",
];

const TXN_DOC_TYPE_VALUES: [&str; 8] = [
    "STOCK_IN",
    "STOCK_OUT",
    "STOCK_TRANSFER",
    "STOCK_TAKE",
    "SERIAL_BATCH",
    "MATERIAL_SERIAL",
    "OPENING",
    "ADJUST",
];

const VOUCHER_SOURCE_VALUES: [&str; 16] = [
    "STOCK_IN",
    "STOCK_OUT",
    "PAYMENT",
    "STOCK_TAKE",
    "SERIAL_BATCH",
    "SERIAL_OUT",
    "SERIAL_SCRAP",
    "OPENING",
    "YEAR_END",
    "MANUAL",
    "FIXED_ASSET_ACQ",
    "FIXED_ASSET_DISPOSAL",
    "DEPRECIATION",
    "EXPENSE_REPORT",
    "EXPENSE_PAYMENT",
    "RECEIPT",
];

const SERIAL_SOURCE_VALUES: [&str; 4] = ["PURCHASE", "TRANSFER", "PRODUCE", "RETURN"];
const IN_TYPE_VALUES: [&str; 3] = ["PURCHASE", "RETURN", "OTHER"];
const OUT_TYPE_VALUES: [&str; 4] = ["ISSUE", "SCRAP", "LOSS", "OTHER"];
const TAKE_TYPE_VALUES: [&str; 2] = ["FULL", "PARTIAL"];

// ===== FIN-1 预算（ARCH-6 第三章）=====

/// `biz_budget.status` 的取值域（与 `state_machine::BUDGET_EDGES` 的节点集合一致）。
const BUDGET_STATUS_VALUES: [&str; 6] = [
    "DRAFT",
    "SUBMITTED",
    "ACTIVE",
    "SUPERSEDED",
    "CLOSED",
    "CANCELLED",
];

/// `biz_budget_usage.status` 的取值域（占用生命周期，ARCH-6 §3.1.3）。
const BUDGET_USAGE_STATUS_VALUES: [&str; 3] = ["OCCUPIED", "CONSUMED", "RELEASED"];

/// `biz_budget_usage.source_type` 的取值域（谁占用预算）。
///
/// 只有两个值：报销提交（`EXPENSE_REPORT`）与采购提交（`PURCHASE_ORDER`）——
/// 这正是 ARCH-6 D4「控制点只有 2 处」在数据层的体现。第三个控制点要先改这里。
const BUDGET_USAGE_SOURCE_VALUES: [&str; 2] = ["EXPENSE_REPORT", "PURCHASE_ORDER"];

// ===== FIN-2 固定资产（ARCH-6 第五章）=====

/// `biz_fixed_asset.status` 的取值域（与 `state_machine::FIXED_ASSET_EDGES` 的节点集合一致）。
const ASSET_STATUS_VALUES: [&str; 4] = ["IN_USE", "IDLE", "DISPOSED", "CANCELLED"];

/// `biz_fixed_asset.disposal_type` 的取值域（`ck_biz_fa_disposal`）。
const ASSET_DISPOSAL_TYPE_VALUES: [&str; 3] = ["SALE", "SCRAP", "LOSS"];

/// `biz_fixed_asset_change.change_type` 的取值域（`ck_biz_fa_chg_type`）。
const ASSET_CHANGE_TYPE_VALUES: [&str; 5] = ["TRANSFER", "CUSTODIAN", "DISPOSAL", "ENABLE", "IDLE"];

/// `biz_depreciation_batch.status` 的取值域（`ck_biz_dep_batch_status`）。
const DEPRECIATION_STATUS_VALUES: [&str; 4] = ["CALCULATED", "POSTED", "REVERSED", "SKIPPED"];

// ===== FIN-3 费用报销（ARCH-6 第四章）=====

/// `biz_expense_report.status` 的取值域（与 `state_machine::EXPENSE_EDGES` 的节点集合一致）。
/// **没有 `REJECTED`**：驳回回 `DRAFT`（员工改完要能再提交）。
const EXPENSE_STATUS_VALUES: [&str; 5] = ["DRAFT", "SUBMITTED", "APPROVED", "PAID", "CANCELLED"];

/// `biz_expense_report.pay_status` 的取值域（派生：`ck_biz_exp_report_pay_match` 保证与金额自洽）。
const EXPENSE_PAY_STATUS_VALUES: [&str; 3] = ["UNPAID", "PARTIAL", "PAID"];

/// `biz_expense_payment.status` 的取值域（`ck_biz_exp_pay_status`）。
const EXPENSE_PAYMENT_STATUS_VALUES: [&str; 2] = ["PAID", "CANCELLED"];

/// `biz_voucher_rule.biz_type` 的取值域：**横跨两根轴**（历史事实，不是设计选择）。
///
/// * 采购入库规则（`PURCHASE_IN_DEFAULT`）用的是**库存变动轴**（`TxnBizType`）；
/// * FIN-2 起的固资 / 折旧规则用的是**凭证来源轴**（`VoucherSourceType`），
///   因为 ARCH-6 §6.4 要求「规则的 `biz_type` 必须与 `VoucherSourceType::as_str()`
///   逐字一致」；折旧与处置的凭证来源本来就是 `DEPRECIATION` /
///   `FIXED_ASSET_ACQ` / `FIXED_ASSET_DISPOSAL`；
/// * FIN-3 报销的 `EXPENSE_REPORT` / `EXPENSE_PAYMENT` 同理（`expense_bridge::SOURCE_*`），
///   它们只属于凭证来源轴，**不在** txn 轴上（报销不产生库存流水）。
///
/// 所以域是两轴的并集（去重），不是「随便放宽」：任何既不在 txn 轴、也不在
/// voucher-source 轴上的值仍然是 FAIL。
const VOUCHER_RULE_BIZ_TYPE_VALUES: [&str; 34] = [
    // txn 轴（22）
    "PURCHASE_IN",
    "PURCHASE_IN_CANCEL",
    "PURCHASE_RETURN",
    "SALE_OUT",
    "PRODUCTION_IN",
    "ADJUST_IN",
    "ADJUST_OUT",
    "TRANSFER_OUT",
    "TRANSFER_IN",
    "ISSUE_OUT",
    "SCRAP_OUT",
    "LOSS_OUT",
    "OTHER_OUT",
    "STOCK_OUT_CANCEL",
    "PRODUCE_CANCEL",
    "SERIAL_IN",
    "SERIAL_OUT",
    "SERIAL_SCRAP",
    "OPENING",
    "STOCK_TAKE_GAIN",
    "STOCK_TAKE_LOSS",
    "STOCK_TAKE_CANCEL",
    // voucher-source 轴（15，去重后 12 个新值；
    // FIN-3 报销的 EXPENSE_REPORT / EXPENSE_PAYMENT 只在这根轴上，不在 txn 轴）
    "STOCK_IN",
    "STOCK_OUT",
    "PAYMENT",
    "STOCK_TAKE",
    "SERIAL_BATCH",
    "YEAR_END",
    "MANUAL",
    "FIXED_ASSET_ACQ",
    "FIXED_ASSET_DISPOSAL",
    "DEPRECIATION",
    "EXPENSE_REPORT",
    "EXPENSE_PAYMENT",
];

/// 需要值域巡检的全部列（**唯一**清单；巡检脚本与 `doc_kind_tests` 按它逐列核对）。
///
/// 域里**不留**已废弃的别名：`biz_status_log.entity_type = 'TRANSFER'` 已被 V0219
/// 统一成 `STOCK_TRANSFER`，出现在库里就是 FAIL（不是「历史兼容」）。
pub const COLUMN_DOMAINS: [ColumnDomain; 24] = [
    ColumnDomain {
        table: "biz_status_log",
        column: "entity_type",
        values: &DOC_KIND_VALUES,
        axis: "① 业务对象",
    },
    ColumnDomain {
        table: "biz_doc_attachment",
        column: "doc_type",
        values: &DOC_KIND_VALUES,
        axis: "① 业务对象",
    },
    ColumnDomain {
        table: "biz_reminder_log",
        column: "doc_type",
        values: &DOC_KIND_VALUES,
        axis: "① 业务对象",
    },
    ColumnDomain {
        table: "biz_flow_def",
        column: "biz_type",
        values: &FLOW_BIZ_TYPE_VALUES,
        axis: "② 审批动作",
    },
    ColumnDomain {
        table: "biz_flow_instance",
        column: "biz_type",
        values: &FLOW_BIZ_TYPE_VALUES,
        axis: "② 审批动作",
    },
    ColumnDomain {
        table: "biz_inventory_txn",
        column: "biz_type",
        values: &TXN_BIZ_TYPE_VALUES,
        axis: "③ 库存变动动作",
    },
    ColumnDomain {
        table: "biz_voucher_rule",
        column: "biz_type",
        values: &VOUCHER_RULE_BIZ_TYPE_VALUES,
        axis: "③ 库存变动动作",
    },
    ColumnDomain {
        table: "biz_inventory_txn",
        column: "doc_type",
        values: &TXN_DOC_TYPE_VALUES,
        axis: "④ 流水来源单据",
    },
    ColumnDomain {
        table: "biz_voucher",
        column: "source_type",
        values: &VOUCHER_SOURCE_VALUES,
        axis: "⑤ 凭证来源",
    },
    ColumnDomain {
        table: "biz_payable",
        column: "source_type",
        values: &VOUCHER_SOURCE_VALUES,
        axis: "⑤ 凭证来源",
    },
    ColumnDomain {
        table: "biz_material_serial",
        column: "source_type",
        values: &SERIAL_SOURCE_VALUES,
        axis: "⑥ 序列号来源",
    },
    ColumnDomain {
        table: "biz_stock_in",
        column: "in_type",
        values: &IN_TYPE_VALUES,
        axis: "⑦ 单据子类型",
    },
    ColumnDomain {
        table: "biz_stock_out",
        column: "out_type",
        values: &OUT_TYPE_VALUES,
        axis: "⑦ 单据子类型",
    },
    ColumnDomain {
        table: "biz_stock_take",
        column: "take_type",
        values: &TAKE_TYPE_VALUES,
        axis: "⑦ 单据子类型",
    },
    // ===== FIN-1 预算 =====
    ColumnDomain {
        table: "biz_budget",
        column: "status",
        values: &BUDGET_STATUS_VALUES,
        axis: "⑧ 预算状态",
    },
    ColumnDomain {
        table: "biz_budget_usage",
        column: "status",
        values: &BUDGET_USAGE_STATUS_VALUES,
        axis: "⑧ 预算状态",
    },
    ColumnDomain {
        table: "biz_budget_usage",
        column: "source_type",
        values: &BUDGET_USAGE_SOURCE_VALUES,
        axis: "⑨ 预算占用来源",
    },
    // ===== FIN-2 固定资产 =====
    ColumnDomain {
        table: "biz_fixed_asset",
        column: "status",
        values: &ASSET_STATUS_VALUES,
        axis: "⑩ 资产状态",
    },
    ColumnDomain {
        table: "biz_fixed_asset",
        column: "disposal_type",
        values: &ASSET_DISPOSAL_TYPE_VALUES,
        axis: "⑩ 资产状态",
    },
    ColumnDomain {
        table: "biz_fixed_asset_change",
        column: "change_type",
        values: &ASSET_CHANGE_TYPE_VALUES,
        axis: "⑩ 资产状态",
    },
    ColumnDomain {
        table: "biz_depreciation_batch",
        column: "status",
        values: &DEPRECIATION_STATUS_VALUES,
        axis: "⑩ 资产状态",
    },
    // ===== FIN-3 费用报销 =====
    ColumnDomain {
        table: "biz_expense_report",
        column: "status",
        values: &EXPENSE_STATUS_VALUES,
        axis: "⑪ 报销状态",
    },
    ColumnDomain {
        table: "biz_expense_report",
        column: "pay_status",
        values: &EXPENSE_PAY_STATUS_VALUES,
        axis: "⑪ 报销状态",
    },
    ColumnDomain {
        table: "biz_expense_payment",
        column: "status",
        values: &EXPENSE_PAYMENT_STATUS_VALUES,
        axis: "⑪ 报销状态",
    },
];

// ==================== 测试残留口径（t_61dca492） ====================

/// 测试残留的**前缀**：`biz_type` 以它开头的行都是 e2e / 验收脚本写进来的，不属于业务。
///
/// 与 `scripts/verify/biz_type_domain.py::is_test_residue` **逐字一致**：
/// 那边判 `value.startswith("E2E_") or value == "D2_SMOKE"`，这边是同一个口径的 Rust 版本；
/// 再加一条 `doc_kind_tests.rs` 的一致性用例钉住，改一边不改另一边会红。
pub const TEST_RESIDUE_PREFIX: &str = "E2E_";

/// 测试残留的**精确值**（没有统一前缀的历史冒烟流程）。
pub const TEST_RESIDUE_EXACT: [&str; 1] = ["D2_SMOKE"];

/// 某个 `biz_type` 值是不是测试残留（`E2E_*` 或 `D2_SMOKE`）。
///
/// ⚠️ 这个判据**不是**「不在词表里就算垃圾」—— `biz_type` 允许客户自定义，
/// 客户的真流程同样不在 [`COLUMN_DOMAINS`] 的域里。两件事分得很清：
/// 词表域回答「这个值是不是内置拼写」，本函数回答「这个值是不是测试脚本写的」。
pub fn is_test_residue(biz_type: &str) -> bool {
    biz_type.starts_with(TEST_RESIDUE_PREFIX) || TEST_RESIDUE_EXACT.contains(&biz_type)
}

/// 与 [`is_test_residue`] 同口径的 **SQL 片段**（列名固定为 `biz_type`）。
///
/// 为什么要有这个常量：启动自检和 `/health` 各写一遍 SQL 迟早写歪，
/// 而 `left(biz_type, 4)` 与 `starts_with` 是同一件事 —— 只在这里写一次。
pub const TEST_RESIDUE_SQL: &str = "(left(biz_type, 4) = 'E2E_' OR biz_type = 'D2_SMOKE')";

#[cfg(test)]
#[path = "doc_kind_tests.rs"]
mod doc_kind_tests;

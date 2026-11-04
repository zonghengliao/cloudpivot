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
}

impl DocKind {
    /// 全部业务对象（顺序 = 声明顺序；前端 TS 常量表与巡检按此顺序逐字比对）。
    pub const ALL: [DocKind; 14] = [
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
pub const ATTACHMENT_DOC_KINDS: [DocKind; 5] = [
    DocKind::Contract,
    DocKind::PurchaseOrder,
    DocKind::StockIn,
    DocKind::Payable,
    DocKind::Payment,
];

/// 能记业务提醒的对象（与 `ck_biz_reminder_log_doc_type` 逐字一致）。
pub const REMINDER_DOC_KINDS: [DocKind; 4] = [
    DocKind::Payable,
    DocKind::PurchaseOrder,
    DocKind::Contract,
    DocKind::Inventory,
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
}

impl FlowBizType {
    /// 全部审批业务类型（顺序 = 声明顺序）。
    pub const ALL: [FlowBizType; 13] = [
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
            FlowBizType::TaskAccept | FlowBizType::Expense | FlowBizType::Other => return None,
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
}

impl TxnBizType {
    /// 全部动账类型（顺序 = 声明顺序）。
    pub const ALL: [TxnBizType; 19] = [
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
    pub const ALL: [TxnDocType; 7] = [
        TxnDocType::StockIn,
        TxnDocType::StockOut,
        TxnDocType::StockTransfer,
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
}

impl VoucherSourceType {
    /// 全部来源（顺序 = 声明顺序）。
    pub const ALL: [VoucherSourceType; 10] = [
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

const DOC_KIND_VALUES: [&str; 14] = [
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
];

const FLOW_BIZ_TYPE_VALUES: [&str; 13] = [
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
];

const TXN_BIZ_TYPE_VALUES: [&str; 19] = [
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
];

const TXN_DOC_TYPE_VALUES: [&str; 7] = [
    "STOCK_IN",
    "STOCK_OUT",
    "STOCK_TRANSFER",
    "SERIAL_BATCH",
    "MATERIAL_SERIAL",
    "OPENING",
    "ADJUST",
];

const VOUCHER_SOURCE_VALUES: [&str; 10] = [
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
];

const SERIAL_SOURCE_VALUES: [&str; 4] = ["PURCHASE", "TRANSFER", "PRODUCE", "RETURN"];
const IN_TYPE_VALUES: [&str; 3] = ["PURCHASE", "RETURN", "OTHER"];
const OUT_TYPE_VALUES: [&str; 4] = ["ISSUE", "SCRAP", "LOSS", "OTHER"];
const TAKE_TYPE_VALUES: [&str; 2] = ["FULL", "PARTIAL"];

/// 需要值域巡检的全部列（**唯一**清单；巡检脚本与 `doc_kind_tests` 按它逐列核对）。
///
/// 域里**不留**已废弃的别名：`biz_status_log.entity_type = 'TRANSFER'` 已被 V0219
/// 统一成 `STOCK_TRANSFER`，出现在库里就是 FAIL（不是「历史兼容」）。
pub const COLUMN_DOMAINS: [ColumnDomain; 14] = [
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
        values: &TXN_BIZ_TYPE_VALUES,
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
];

#[cfg(test)]
#[path = "doc_kind_tests.rs"]
mod doc_kind_tests;

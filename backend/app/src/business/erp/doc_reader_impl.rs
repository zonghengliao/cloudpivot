//! ERP 侧对 [`crate::business::flow::doc_reader::DocReader`] 的实现与启动期注册（P2-33）。
//!
//! # 这个文件解决什么
//!
//! P2-32 在 `flow` 侧立了接缝：`DocReader` trait + 进程级注册表，并且接缝文件里
//! **不许出现任何业务域路径**。本文件是它的另一半 —— 实现在**业务域**里：
//!
//! ```text
//! 改前：flow ──直连 135 行──→ erp          （erp→flow 132 处，双向 = 环）
//! P2-32：flow ──trait/注册表──→ (抽象)                    ← 接缝就位，仍直连
//! P2-33：                              erp ──实现+注册──→ 抽象   ← 本文件（新增一条 erp→flow
//!                                                              的「同向」边，环的另外半边由
//!                                                              P2-34 摘掉）
//! P2-34：flow 的 99 行直连改走注册表                        ← 消边
//! ```
//!
//! **本卡不消 `flow → erp` 这条边**（消边是 P2-34 的活）：本卡交付的是「实现已经在业务域
//! 里、启动期已经进了注册表」，所以 P2-34 只要把 99 个直连调用点换成 4 个注册表调用即可，
//! 不用再写一行实现。与 P2-31 / P2-32 同口径：**单 crate 下 `cargo tree` 表达不了模块边**，
//! 边数证据走 `scripts/lint/module_cycle_report.py` 与 `flow::doc_reader_tests` 的燃烧率闸门。
//!
//! # 一条审批一个实现（16 条）
//!
//! 注册键是 [`FlowBizType`]（`biz_flow_instance.biz_type` 的落库值），
//! **不是** `DocKind`：同一张收货单有 `STOCK_IN_POST` / `STOCK_IN_CANCEL` 两条审批，
//! 两条的制单人查法与回调函数都不同（见 P2-32 的模块注释）。所以这里是 16 个实现，
//! 与 `flow::internal_control::of_biz` 今天 `governed` 的 16 个分支**逐条对齐**：
//!
//! | # | `biz_type` | 单据表 | 单号列 | 状态列 | 制单人列 | 回调（`on_settled`） |
//! |:--:|:--|:--|:--|:--|:--|:--|
//! | 1 | `PO_APPROVE` | `biz_purchase_order` | `order_no` | `status` | `create_by` | [`purchase_bridge::on_po_settled`] |
//! | 2 | `STOCK_IN_POST` | `biz_stock_in` | `in_no` | `status` | `create_by` | [`stockin_bridge::on_post_settled`] |
//! | 3 | `STOCK_IN_CANCEL` | `biz_stock_in` | `in_no` | `status` | `create_by` | [`stockin_bridge::on_cancel_settled`] |
//! | 4 | `STOCK_TAKE_POST` | `biz_stock_take` | `take_no` | `status` | `create_by` | [`stocktake_bridge::on_stock_take_settled`] |
//! | 5 | `PAYABLE_PAY` | `biz_payment` | `payment_no` | `approval_status` | `create_by` | [`payable_bridge::on_payment_settled`] |
//! | 6 | `PAYABLE_CANCEL` | `biz_payable` | `payable_no` | `status` | `create_by` | [`payable_bridge::on_cancel_settled`] |
//! | 7 | `VOUCHER_REVERSE` | `biz_voucher` | `voucher_no` | `status` | `create_by` | [`voucher_bridge::on_reverse_settled`] |
//! | 8 | `CONTRACT_APPROVE` | `biz_contract` | `contract_no` | `status` | `create_by` | [`contract_bridge::on_contract_settled`] |
//! | 9 | `BUDGET_APPROVE` | `biz_budget` | `budget_no` | `status` | `create_by` | [`budget_bridge::on_budget_settled`] |
//! | 10 | `ASSET_DISPOSAL` | `biz_fixed_asset` | `asset_no` | `status` | `create_by` | [`asset_bridge::on_disposal_settled`] |
//! | 11 | `EXPENSE` | `biz_expense_report` | `report_no` | `status` | **`reporter_id`** | [`expense_bridge::on_expense_settled`] |
//! | 12 | `SO_APPROVE` | `biz_sales_order` | `so_no` | `status` | `create_by` | [`sales_approval_bridge::on_so_settled`] |
//! | 13 | `PR_APPROVE` | `biz_purchase_requisition` | `req_no` | `status` | `create_by` | [`requisition_bridge::on_pr_settled`] |
//! | 14 | `PRT_APPROVE` | `biz_purchase_return` | `return_no` | `status` | `create_by` | [`purchase_return_bridge::on_settled`] |
//! | 15 | `COST_SETTLE` | `biz_cost_period` | `batch_no` | `status` | `create_by` | [`cost_period_bridge::on_cost_settle_decided`] |
//! | 16 | `RECEIPT_REFUND` | `biz_receipt_refund` | `refund_no` | `status` | `create_by` | [`receipt_refund_bridge::on_refund_settled`] |
//! | 17 | `STOCK_TRANSFER` | `biz_stock_transfer` | `transfer_no` | `status` | `create_by` | [`transfer_approval_settle::on_transfer_settled`] |
//! | 18 | `QUOTATION_APPROVE` | `biz_quotation` | `quote_no` | `status` | `create_by` | [`quotation_approval_bridge::on_quotation_settled`] |
//! | 19 | `PP_APPROVE` | `biz_purchase_plan` | `plan_no` | `status` | `create_by` | [`purchase_plan_bridge::on_pp_settled`] |
//! | 20 | `SUPPLIER_ACCESS` | `biz_supplier_access` | `access_no` | `status` | `create_by` | [`supplier_access_flow::on_sa_settled`] |
//! | 21 | `FWA_APPROVE` | `biz_framework_agreement` | `agreement_no` | `status` | `create_by` | [`framework_agreement_flow::on_fwa_settled`] |
//!
//! **第 17 行是 P2-34 补的**：调拨单的终态回调原来住在 `flow::task_bridge`（直连 ERP），
//! 搬进 ERP 后需要一个入口被调起 —— 注册表就是那个入口。它**不在**内控受控集合里
//! （`governed=false`），所以 `guard` 放行、`creator` 为 `None`，与改造前行为一致。
//!
//! 三条与上表相关的口径：
//!
//! - **第 11 行的制单人列是 `reporter_id`**。其余 15 张单据的「制单人」都是 `create_by`，
//!   报销单例外（报销人是 `reporter_id`，`create_by` 可能是代录的财务）——
//!   这不是笔误，`expense_bridge::internal_control_creator` 今天就查 `reporter_id`。
//!   快照的 `creator_id` 必须与内控闸同源，否则「详情里显示的制单人」和
//!   「被排除的那个人」会对不上。
//! - **第 5 行的状态列是 `approval_status`**：付款台账 `biz_payment` 没有 `status` 列，
//!   它只有审批态（`NONE` / `PENDING` / `APPROVED` / `REJECTED`），账务态在应付单上。
//! - **第 2、3 行共用同一张表**：过账与作废挂在同一张收货单上，两个实现、两个键、
//!   两个回调，但单据快照是同一份。
//!
//! # 三个方法都**委派**给现有桥，不重写一行
//!
//! | 方法 | 实现 |
//! |:--|:--|
//! | `on_settled` | 直接调该桥现有的 `on_*_settled`（幂等 / 自过滤 / 失败只记日志三件事它们早就做对了） |
//! | `guard_self_approval` | 直接调该桥现有的 `guard_self_approval`（内控闸连着 `separation` 开关与会签口径，重写必然分叉） |
//! | `internal_control_creator` | 直接调该桥现有的 `internal_control_creator`（`PAYABLE_CANCEL` 走 `_of_payable` 变体） |
//! | `doc_status` | **新增**：唯一没有现成函数的一件事（见下） |
//!
//! 「委派」不是偷懒：P2-34 要把 `flow` 的 99 行直连换掉，那些行调的就是这些函数。
//! 如果这里重写一套，等价性就变成「两套逻辑碰巧一样」；委派之后是**同一套逻辑走两条路**，
//! 替换前后行为逐字相同（`flow::doc_reader_tests` 的用例钉住了「未注册 → 降级」那一侧）。
//!
//! # `doc_status` 是唯一新写的查询：为什么用「表驱动」而不是 16 段 `if`
//!
//! 16 张单据表的「单号 / 状态 / 制单人」三列位置不同但形态一致（全部 `varchar NOT NULL`
//! + `create_by` 可空 + 都有 `deleted_at`），所以三列 + 表名 + 文案函数做成一张
//! [`DOC_SPECS`] 常量表，查询只写一遍：
//!
//! - **SQL 拼接安全**：拼进去的四段全部来自 [`DOC_SPECS`] 的 `&'static str` 常量，
//!   没有任何一段来自请求参数（`doc_id` 走 `?::uuid` 绑定）；
//! - **`deleted_at IS NULL`**：软删的单据对流程侧等于「不存在」→ `Ok(None)`，
//!   审批详情显示「单据已删除」而不是把已删单的状态当成现状；
//! - **状态原值 + 中文文案分开返回**：调用方拿 `status` 与状态机的边逐字比对，
//!   拿 `status_label` 直接打给用户（P2-32 的 `DocSnapshot` 契约）。
//!
//! # 不注册的 `biz_type`（如实说明，不是漏了）
//!
//! | `biz_type` | 为什么不在这里 |
//! |:--|:--|
//! | `STOCK_TRANSFER` | 调拨审批的回调住在 `flow::task_bridge`（任务与调拨共用一条入口），今天也**不在** `internal_control` 的 `governed` 名单里 —— 本卡照旧不注册，行为不变；要不要搬进 ERP 是 P2-34 的判断 |
//! | `TASK_ACCEPT` / `FLOW_EDGE_CONFIG` / `OTHER` | 不是 ERP 单据（任务验收 / 状态机配置 / 客户端自建流程），本来就不受内控约束 |
//! | `HR_*`（4 条） | 人力资源域的实现在 `business::hr`，属另一张卡 |
//!
//! # 两处**既有缺口**，本卡如实沿用（补闸要单独开卡）
//!
//! 1. `STOCK_IN_CANCEL`：`stockin_bridge::guard_self_approval` 内部只认 `STOCK_IN_POST`
//!    （`if instance.biz_type != POST_BIZ_TYPE { return Ok(()) }`），所以**收货作废审批今天
//!    没有内控闸**。本实现的 `guard_self_approval` 委派同一个函数 → 等价于「放行」，
//!    与今天逐字一致；
//! 2. `PAYABLE_CANCEL` 有闸（`payable_bridge::guard_self_approval` 内部对
//!    `CANCEL_BIZ_TYPE` 单列了一段），所以它照常受控。
//!
//! 把缺口 1 补上会**改变生产行为**（多拦一批人），不属于「解环」的范围：本卡的任何
//! 行为变化都必须能用「等价性」解释，补闸不能。缺口与理由写在这里，避免下一个人把它
//! 当成「P2-33 抄漏了」。

use std::sync::Arc;

use async_trait::async_trait;
use cloudpivot_doc::FlowBizType;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::asset::{asset_bridge, asset_dto};
use crate::business::erp::budget::{budget_bridge, budget_dto};
use crate::business::erp::contract::{contract_bridge, contract_model};
use crate::business::erp::cost::{cost_period_bridge, cost_period_dto, cost_standard_dto, cost_standard_publish};
use crate::business::erp::expense::{expense_bridge, expense_dto};
use crate::business::erp::payable::{payable_bridge, payable_payment, payable_query};
use crate::business::erp::purchase::{
    framework_agreement_flow, framework_agreement_model, purchase_bridge, purchase_model, purchase_plan_bridge,
    purchase_plan_dto, purchase_requisition_dto, purchase_return_bridge, purchase_return_dto, supplier_access_flow,
    supplier_access_model,
};
use crate::business::erp::sales::{quotation_approval_bridge, quotation_model, sales_approval_bridge, sales_model};
use crate::business::erp::stockin::{stockin_base, stockin_bridge};
use crate::business::erp::stocktake::{stocktake_bridge, stocktake_model};
use crate::business::erp::transfer::{transfer_approval_settle, transfer_model};
use crate::business::erp::voucher::{voucher_bridge, voucher_query};
use crate::business::erp::{receipt_refund, receipt_refund_bridge, requisition_bridge};
use crate::business::flow::doc_reader::{self, DocReader, DocRegistry, DocSnapshot};

/// 一张单据表的「快照取法」（见模块注释「为什么用表驱动」）。
///
/// 五个字段全部是**常量**：SQL 里拼进去的每一段都来自编译期字面量，
/// 没有任何一段来自请求参数（唯一的入参 `doc_id` 走 `?::uuid` 绑定）。
#[derive(Debug, Clone, Copy)]
pub struct DocSpec {
    /// 单据表名（`biz_*`，白名单常量）。
    pub table: &'static str,
    /// 单号列（`order_no` / `in_no` / …）。
    pub doc_no: &'static str,
    /// 状态列。付款台账是 `approval_status`（没有 `status` 列）。
    pub status: &'static str,
    /// 制单人列。报销单是 `reporter_id`（报销人 ≠ 代录人）。
    pub creator: &'static str,
    /// 状态中文文案。入参是落库原值，返回展示文案。
    pub label: fn(&str) -> String,
}

impl DocSpec {
    /// 按单据 id 取快照。`Ok(None)` = 单据不存在或已软删（数据事实，不是错误）。
    pub async fn snapshot(&self, doc_id: Uuid) -> DbResult<Option<DocSnapshot>> {
        let sql = format!(
            "SELECT {} AS doc_no, {} AS status, {} AS creator_id FROM {} \
              WHERE id = ?::uuid AND deleted_at IS NULL",
            self.doc_no, self.status, self.creator, self.table
        );
        // SCOPE-EXEMPT: 按主键精确取 1 行，不做数据范围裁剪 —— 流程侧要判断「这张单是什么状态」
        // 才能决定往哪儿走，裁剪会让「看不到单据的人」把单据当成不存在（P2-32 的接缝契约）。
        let rows = db().find_with_params(&sql, vec![Value::from(&doc_id)]).await?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let status: String = row.get_typed("status")?;
        Ok(Some(DocSnapshot {
            doc_no: row.get_typed("doc_no")?,
            status_label: (self.label)(&status),
            status,
            creator_id: row.get_typed("creator_id")?,
        }))
    }
}

/// 一个 ERP 单据审批实现（= 一个 `biz_type`）。
///
/// 用枚举而不是 16 个 struct：三个委派方法都是「按单据选桥函数」，
/// 枚举 + `match` 一屏就能看全，拼错名字编译期就红（而 `fn` 指针表拼错只能是运行期）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErpDoc {
    /// 采购订单审批（`PO_APPROVE`）。
    PurchaseOrder,
    /// 收货过账审批（`STOCK_IN_POST`）。
    StockInPost,
    /// 收货作废审批（`STOCK_IN_CANCEL`）。
    StockInCancel,
    /// 盘点过账审批（`STOCK_TAKE_POST`）。
    StockTakePost,
    /// 应付付款审批（`PAYABLE_PAY`）。
    PayablePay,
    /// 应付作废审批（`PAYABLE_CANCEL`）。
    PayableCancel,
    /// 凭证红冲审批（`VOUCHER_REVERSE`）。
    VoucherReverse,
    /// 合同审批（`CONTRACT_APPROVE`）。
    ContractApprove,
    /// 预算编制 / 调整审批（`BUDGET_APPROVE`）。
    BudgetApprove,
    /// 资产处置审批（`ASSET_DISPOSAL`）。
    AssetDisposal,
    /// 费用报销审批（`EXPENSE`）。
    Expense,
    /// 销售订单审批（`SO_APPROVE`）。
    SoApprove,
    /// 采购申请审批（`PR_APPROVE`）。
    PrApprove,
    /// 采购退货审批（`PRT_APPROVE`）。
    PrtApprove,
    /// 成本结算审批（`COST_SETTLE`）。
    CostSettle,
    /// 收款退款审批（`RECEIPT_REFUND`）。
    ReceiptRefund,
    /// 库存调拨审批（`STOCK_TRANSFER`）。
    ///
    /// **只注册终态回调，不参与内控排除**（P2-34）：回调原来住在 `flow::task_bridge`，
    /// 搬进 ERP 后由注册表调起；而「制单人不能批自己那一票」这条约束今天**不覆盖**调拨单
    /// （`flow::internal_control::GOVERNED_CODES` 里没有它），所以本变体的 `guard` 是放行、
    /// `creator` 是 `None` —— 与改造前逐字一致。
    StockTransfer,
    /// 销售报价单审批（`QUOTATION_APPROVE`，销售链 M3-04）。
    ///
    /// 审批是「发出」（`DRAFT → SENT`）的前置条件，不是单据状态；通过回调**自动发出**，
    /// 驳回 / 撤回留在草稿。受内控约束（制单人不可自审，`GOVERNED_CODES` 已登记）。
    QuotationApprove,
    /// 采购计划审批（`PP_APPROVE`，采购链 M4-02）。
    ///
    /// 年度 / 月度采购计划提交审批：通过即 `APPROVED`（可转采购申请），
    /// 驳回 / 撤回打回 `DRAFT`。受内控约束（制单人不可自审，`GOVERNED_CODES` 已登记）。
    PurchasePlan,
    /// 供应商准入 / 黑名单审批（`SUPPLIER_ACCESS`，采购链 M4-14）。
    ///
    /// 通过即把申请单置 `APPROVED` 并落 `biz_supplier.status`（拉黑 → 停用，准入/解除 → 启用）；
    /// 驳回 / 撤回打回 `DRAFT`，供应商状态一个字节不动。受内控约束（申请人不可自审）。
    SupplierAccess,
    /// 框架协议审批（`FWA_APPROVE`，采购链 M4-11b）。
    ///
    /// 提交审批：通过即 `APPROVED`（协议生效、可被下单引用），驳回 / 撤回打回 `DRAFT`。
    /// 受内控约束（制单人不可自审自己签的协议）。
    FrameworkAgreement,
    /// 标准成本卡发布审批（`COST_STANDARD_PUBLISH`，成本核算 M5-10 / V0465）。
    ///
    /// 「审核后生效」：通过 → `publish_state = PUBLISHED` + `status = 1`（参与差异分析取数）；
    /// 驳回 / 撤回 → `DRAFT`（清空实例指针，可改后重提）。受内控约束（制单人不可自审自己建的卡）。
    CostStandardPublish,
}

impl ErpDoc {
    /// 全部实现（= 注册表里应当出现的 22 个键，顺序 = 模块注释表格的顺序 + 调拨单）。
    ///
    /// 21 个受内控约束 + 1 个只接终态回调（`StockTransfer`，见该变体的注释）。
    pub const ALL: [ErpDoc; 22] = [
        ErpDoc::PurchaseOrder,
        ErpDoc::StockInPost,
        ErpDoc::StockInCancel,
        ErpDoc::StockTakePost,
        ErpDoc::PayablePay,
        ErpDoc::PayableCancel,
        ErpDoc::VoucherReverse,
        ErpDoc::ContractApprove,
        ErpDoc::BudgetApprove,
        ErpDoc::AssetDisposal,
        ErpDoc::Expense,
        ErpDoc::SoApprove,
        ErpDoc::PrApprove,
        ErpDoc::PrtApprove,
        ErpDoc::CostSettle,
        ErpDoc::ReceiptRefund,
        ErpDoc::StockTransfer,
        ErpDoc::QuotationApprove,
        ErpDoc::PurchasePlan,
        ErpDoc::SupplierAccess,
        ErpDoc::FrameworkAgreement,
        ErpDoc::CostStandardPublish,
    ];

    /// 审批业务类型（注册表键）。
    pub const fn biz_type(self) -> FlowBizType {
        match self {
            ErpDoc::PurchaseOrder => FlowBizType::PoApprove,
            ErpDoc::StockInPost => FlowBizType::StockInPost,
            ErpDoc::StockInCancel => FlowBizType::StockInCancel,
            ErpDoc::StockTakePost => FlowBizType::StockTakePost,
            ErpDoc::PayablePay => FlowBizType::PayablePay,
            ErpDoc::PayableCancel => FlowBizType::PayableCancel,
            ErpDoc::VoucherReverse => FlowBizType::VoucherReverse,
            ErpDoc::ContractApprove => FlowBizType::ContractApprove,
            ErpDoc::BudgetApprove => FlowBizType::BudgetApprove,
            ErpDoc::AssetDisposal => FlowBizType::AssetDisposal,
            ErpDoc::Expense => FlowBizType::Expense,
            ErpDoc::SoApprove => FlowBizType::SoApprove,
            ErpDoc::PrApprove => FlowBizType::PrApprove,
            ErpDoc::PrtApprove => FlowBizType::PrtApprove,
            ErpDoc::CostSettle => FlowBizType::CostSettle,
            ErpDoc::ReceiptRefund => FlowBizType::ReceiptRefund,
            ErpDoc::StockTransfer => FlowBizType::StockTransfer,
            ErpDoc::QuotationApprove => FlowBizType::QuotationApprove,
            ErpDoc::PurchasePlan => FlowBizType::PpApprove,
            ErpDoc::SupplierAccess => FlowBizType::SupplierAccess,
            ErpDoc::FrameworkAgreement => FlowBizType::FrameworkAgreement,
            ErpDoc::CostStandardPublish => FlowBizType::CostStandardPublish,
        }
    }

    /// 单据名（日志与断言里给人看）。
    pub const fn doc_name(self) -> &'static str {
        match self {
            ErpDoc::PurchaseOrder => "采购订单",
            ErpDoc::StockInPost => "收货单（过账审批）",
            ErpDoc::StockInCancel => "收货单（作废审批）",
            ErpDoc::StockTakePost => "盘点单",
            ErpDoc::PayablePay => "付款记录",
            ErpDoc::PayableCancel => "应付单",
            ErpDoc::VoucherReverse => "凭证",
            ErpDoc::ContractApprove => "合同",
            ErpDoc::BudgetApprove => "预算",
            ErpDoc::AssetDisposal => "资产卡片",
            ErpDoc::Expense => "报销单",
            ErpDoc::SoApprove => "销售订单",
            ErpDoc::PrApprove => "采购申请",
            ErpDoc::PrtApprove => "采购退货单",
            ErpDoc::CostSettle => "成本核算批次",
            ErpDoc::ReceiptRefund => "收款退款单",
            ErpDoc::StockTransfer => "调拨单",
            ErpDoc::QuotationApprove => "报价单",
            ErpDoc::PurchasePlan => "采购计划",
            ErpDoc::SupplierAccess => "供应商准入申请",
            ErpDoc::FrameworkAgreement => "框架协议",
            ErpDoc::CostStandardPublish => "标准成本卡",
        }
    }

    /// 快照取法（表 / 列 / 文案函数）。
    pub const fn spec(self) -> DocSpec {
        // 15 张表的「制单人」都是 create_by；报销单是 reporter_id（见模块注释）。
        // 三列全部 `varchar NOT NULL` + `create_by` 可空 + 都带 `deleted_at`：
        // 这是 2026-11-15 在 `cloudpivot_test`（181 表模板库）上逐列核对过的。
        match self {
            ErpDoc::PurchaseOrder => DocSpec {
                table: "biz_purchase_order",
                doc_no: "order_no",
                status: "status",
                creator: "create_by",
                label: |status| purchase_model::status_label(status).to_string(),
            },
            ErpDoc::StockInPost | ErpDoc::StockInCancel => DocSpec {
                table: "biz_stock_in",
                doc_no: "in_no",
                status: "status",
                creator: "create_by",
                label: |status| stockin_base::status_label(status).to_string(),
            },
            ErpDoc::StockTakePost => DocSpec {
                table: "biz_stock_take",
                doc_no: "take_no",
                status: "status",
                creator: "create_by",
                label: |status| stocktake_model::status_label(status).to_string(),
            },
            ErpDoc::PayablePay => DocSpec {
                table: "biz_payment",
                doc_no: "payment_no",
                status: "approval_status",
                creator: "create_by",
                label: |status| payable_payment::approval_status_label(status).to_string(),
            },
            ErpDoc::PayableCancel => DocSpec {
                table: "biz_payable",
                doc_no: "payable_no",
                status: "status",
                creator: "create_by",
                label: |status| payable_query::status_label(status).to_string(),
            },
            ErpDoc::VoucherReverse => DocSpec {
                table: "biz_voucher",
                doc_no: "voucher_no",
                status: "status",
                creator: "create_by",
                label: |status| voucher_query::status_label(status).to_string(),
            },
            ErpDoc::ContractApprove => DocSpec {
                table: "biz_contract",
                doc_no: "contract_no",
                status: "status",
                creator: "create_by",
                label: |status| contract_model::status_label(status).to_string(),
            },
            ErpDoc::BudgetApprove => DocSpec {
                table: "biz_budget",
                doc_no: "budget_no",
                status: "status",
                creator: "create_by",
                label: |status| budget_dto::status_label(status).to_string(),
            },
            ErpDoc::AssetDisposal => DocSpec {
                table: "biz_fixed_asset",
                doc_no: "asset_no",
                status: "status",
                creator: "create_by",
                label: |status| asset_dto::status_label(status).to_string(),
            },
            ErpDoc::Expense => DocSpec {
                table: "biz_expense_report",
                doc_no: "report_no",
                status: "status",
                creator: "reporter_id",
                label: |status| expense_dto::status_label(status).to_string(),
            },
            ErpDoc::SoApprove => DocSpec {
                table: "biz_sales_order",
                doc_no: "so_no",
                status: "status",
                creator: "create_by",
                label: |status| sales_model::status_label(status).to_string(),
            },
            ErpDoc::PrApprove => DocSpec {
                table: "biz_purchase_requisition",
                doc_no: "req_no",
                status: "status",
                creator: "create_by",
                label: |status| purchase_requisition_dto::status_label(status).to_string(),
            },
            ErpDoc::PrtApprove => DocSpec {
                table: "biz_purchase_return",
                doc_no: "return_no",
                status: "status",
                creator: "create_by",
                label: |status| purchase_return_dto::status_label(status).to_string(),
            },
            ErpDoc::CostSettle => DocSpec {
                table: "biz_cost_period",
                doc_no: "batch_no",
                status: "status",
                creator: "create_by",
                label: |status| cost_period_dto::status_label(status).to_string(),
            },
            ErpDoc::StockTransfer => DocSpec {
                // 表名是 `biz_stock_transfer`（不是 `biz_transfer`）：V0140 建表，
                // `doc_spec_identifiers_exist_in_migrations` 会在迁移里逐字核对。
                table: "biz_stock_transfer",
                doc_no: "transfer_no",
                status: "status",
                creator: "create_by",
                label: |status| transfer_model::status_label(status).to_string(),
            },
            ErpDoc::ReceiptRefund => DocSpec {
                table: "biz_receipt_refund",
                doc_no: "refund_no",
                status: "status",
                creator: "create_by",
                label: |status| receipt_refund::status_label(status).to_string(),
            },
            ErpDoc::QuotationApprove => DocSpec {
                table: "biz_quotation",
                doc_no: "quote_no",
                status: "status",
                creator: "create_by",
                label: |status| quotation_model::status_label(status).to_string(),
            },
            ErpDoc::PurchasePlan => DocSpec {
                table: "biz_purchase_plan",
                doc_no: "plan_no",
                status: "status",
                creator: "create_by",
                label: |status| purchase_plan_dto::status_label(status).to_string(),
            },
            ErpDoc::SupplierAccess => DocSpec {
                table: "biz_supplier_access",
                doc_no: "access_no",
                status: "status",
                creator: "create_by",
                label: |status| supplier_access_model::status_label(status).to_string(),
            },
            ErpDoc::FrameworkAgreement => DocSpec {
                table: "biz_framework_agreement",
                doc_no: "agreement_no",
                status: "status",
                creator: "create_by",
                label: |status| framework_agreement_model::status_label(status).to_string(),
            },
            // 标准成本卡**没有单号列**（V0374 的卡头只有物料 + 版本号）：`doc_no` 用列表达式
            // 拼一个可读标识 `STD-v<版本号>`；`status` 列取**发布轴**（流程侧要判的是
            // 「这张卡还在等审批吗」，`status` 的 1/0 是取数轴，判不了审批 —— 见 V0465 头注释）。
            ErpDoc::CostStandardPublish => DocSpec {
                table: "biz_cost_standard",
                doc_no: "('STD-v' || version_no::text)",
                status: "publish_state",
                creator: "create_by",
                label: |state| cost_standard_dto::publish_state_label(state).to_string(),
            },
        }
    }

    /// 终态回调：委派给该桥现有的 `on_*_settled`（幂等 / 自过滤 / 失败只记日志它们已做对）。
    ///
    /// 返回值（受影响单据 id）在这里**丢掉**：`DocReader::on_settled` 没有错误通道，
    /// 桥自己已经打过 `info` / `warn` 日志，这里再编一个返回值没有消费者。
    pub async fn settle(self, instance_id: Option<Uuid>, instance_status: &str) {
        match self {
            ErpDoc::PurchaseOrder => {
                let _ = purchase_bridge::on_po_settled(instance_id, instance_status).await;
            }
            ErpDoc::StockInPost => {
                let _ = stockin_bridge::on_post_settled(instance_id, instance_status).await;
            }
            ErpDoc::StockInCancel => {
                let _ = stockin_bridge::on_cancel_settled(instance_id, instance_status).await;
            }
            ErpDoc::StockTakePost => {
                let _ = stocktake_bridge::on_stock_take_settled(instance_id, instance_status).await;
            }
            ErpDoc::PayablePay => {
                let _ = payable_bridge::on_payment_settled(instance_id, instance_status).await;
            }
            ErpDoc::PayableCancel => {
                let _ = payable_bridge::on_cancel_settled(instance_id, instance_status).await;
            }
            ErpDoc::VoucherReverse => {
                let _ = voucher_bridge::on_reverse_settled(instance_id, instance_status).await;
            }
            ErpDoc::ContractApprove => {
                let _ = contract_bridge::on_contract_settled(instance_id, instance_status).await;
            }
            ErpDoc::BudgetApprove => {
                let _ = budget_bridge::on_budget_settled(instance_id, instance_status).await;
            }
            ErpDoc::AssetDisposal => {
                let _ = asset_bridge::on_disposal_settled(instance_id, instance_status).await;
            }
            ErpDoc::Expense => {
                let _ = expense_bridge::on_expense_settled(instance_id, instance_status).await;
            }
            ErpDoc::SoApprove => {
                let _ = sales_approval_bridge::on_so_settled(instance_id, instance_status).await;
            }
            ErpDoc::PrApprove => {
                let _ = requisition_bridge::on_pr_settled(instance_id, instance_status).await;
            }
            ErpDoc::PrtApprove => {
                let _ = purchase_return_bridge::on_settled(instance_id, instance_status).await;
            }
            ErpDoc::CostSettle => {
                let _ = cost_period_bridge::on_cost_settle_decided(instance_id, instance_status).await;
            }
            ErpDoc::ReceiptRefund => {
                let _ = receipt_refund_bridge::on_refund_settled(instance_id, instance_status).await;
            }
            ErpDoc::StockTransfer => {
                // P2-34：回调本体从 `flow::task_bridge` 搬进 ERP（`transfer_approval_settle`）。
                let _ = transfer_approval_settle::on_transfer_settled(instance_id, instance_status).await;
            }
            ErpDoc::QuotationApprove => {
                let _ = quotation_approval_bridge::on_quotation_settled(instance_id, instance_status).await;
            }
            ErpDoc::PurchasePlan => {
                let _ = purchase_plan_bridge::on_pp_settled(instance_id, instance_status).await;
            }
            ErpDoc::SupplierAccess => {
                let _ = supplier_access_flow::on_sa_settled(instance_id, instance_status).await;
            }
            ErpDoc::FrameworkAgreement => {
                let _ = framework_agreement_flow::on_fwa_settled(instance_id, instance_status).await;
            }
            ErpDoc::CostStandardPublish => {
                let _ = cost_standard_publish::on_publish_decided(instance_id, instance_status).await;
            }
        }
    }

    /// 内控闸：委派给该桥现有的 `guard_self_approval`。
    ///
    /// ⚠️ 桥内部会按 `instance.biz_type` **再过滤一次**，所以两条共用同一个桥函数的审批
    /// （收货过账 / 作废）拿到的判定并不相同：作废那条今天就是「放行」，见模块注释
    /// 「两处既有缺口」。这里**不**把它改成统一拦截 —— 那会改变生产行为。
    pub async fn guard(self, task_id: Uuid, operator: Uuid) -> std::result::Result<(), ApiError> {
        match self {
            ErpDoc::PurchaseOrder => purchase_bridge::guard_self_approval(task_id, operator).await,
            ErpDoc::StockInPost | ErpDoc::StockInCancel => stockin_bridge::guard_self_approval(task_id, operator).await,
            ErpDoc::StockTakePost => stocktake_bridge::guard_self_approval(task_id, operator).await,
            ErpDoc::PayablePay | ErpDoc::PayableCancel => payable_bridge::guard_self_approval(task_id, operator).await,
            ErpDoc::VoucherReverse => voucher_bridge::guard_self_approval(task_id, operator).await,
            ErpDoc::ContractApprove => contract_bridge::guard_self_approval(task_id, operator).await,
            ErpDoc::BudgetApprove => budget_bridge::guard_self_approval(task_id, operator).await,
            ErpDoc::AssetDisposal => asset_bridge::guard_self_approval(task_id, operator).await,
            ErpDoc::Expense => expense_bridge::guard_self_approval(task_id, operator).await,
            ErpDoc::SoApprove => sales_approval_bridge::guard_self_approval(task_id, operator).await,
            ErpDoc::PrApprove => requisition_bridge::guard_self_approval(task_id, operator).await,
            ErpDoc::PrtApprove => purchase_return_bridge::guard_self_approval(task_id, operator).await,
            ErpDoc::CostSettle => cost_period_bridge::guard_self_approval(task_id, operator).await,
            ErpDoc::ReceiptRefund => receipt_refund_bridge::guard_self_approval(task_id, operator).await,
            // 调拨单今天没有内控闸（`flow::internal_control::GOVERNED_CODES` 里没有它）。
            // 补闸 = 多拦一批人 = 改变生产行为，不属于「解环」，所以这里显式放行。
            ErpDoc::StockTransfer => Ok(()),
            ErpDoc::QuotationApprove => quotation_approval_bridge::guard_self_approval(task_id, operator).await,
            ErpDoc::PurchasePlan => purchase_plan_bridge::guard_self_approval(task_id, operator).await,
            ErpDoc::SupplierAccess => supplier_access_flow::guard_self_approval(task_id, operator).await,
            ErpDoc::FrameworkAgreement => framework_agreement_flow::guard_self_approval(task_id, operator).await,
            ErpDoc::CostStandardPublish => cost_standard_publish::guard_self_approval(task_id, operator).await,
        }
    }

    /// 审批点「同意」前的业务前置闸（M3-11：客户信用额度）。
    ///
    /// 18 条审批线里**只有销售订单**在 `APPROVED` 时刻占用客户信用额度（`ARCH-7 §15.5.2`），
    /// 所以只有它覆盖；其余显式列出并放行 —— 新增一条 `ErpDoc` 变体时 `match` 不全会编译失败，
    /// 逼着人想一遍「这条审批线通过时会不会动硬额度」。
    pub async fn guard_approve(self, task_id: Uuid) -> std::result::Result<(), ApiError> {
        match self {
            ErpDoc::SoApprove => sales_approval_bridge::guard_credit_on_approve(task_id).await,
            ErpDoc::PurchaseOrder
            | ErpDoc::StockInPost
            | ErpDoc::StockInCancel
            | ErpDoc::StockTakePost
            | ErpDoc::PayablePay
            | ErpDoc::PayableCancel
            | ErpDoc::VoucherReverse
            | ErpDoc::ContractApprove
            | ErpDoc::BudgetApprove
            | ErpDoc::AssetDisposal
            | ErpDoc::Expense
            | ErpDoc::PrApprove
            | ErpDoc::PrtApprove
            | ErpDoc::CostSettle
            | ErpDoc::ReceiptRefund
            | ErpDoc::StockTransfer
            | ErpDoc::QuotationApprove
            | ErpDoc::PurchasePlan
            | ErpDoc::SupplierAccess
            | ErpDoc::FrameworkAgreement
            | ErpDoc::CostStandardPublish => Ok(()),
        }
    }

    /// 内控用制单人：委派给该桥现有的 `internal_control_creator`。
    ///
    /// `PAYABLE_CANCEL` 走 `_of_payable` 变体：作废审批的 `biz_id` 是**应付单** id，
    /// 不是付款记录 id —— 拿错表的 `create_by` 会排除错人（内控事故）。
    pub async fn creator(self, biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
        match self {
            ErpDoc::PurchaseOrder => purchase_bridge::internal_control_creator(biz_id).await,
            ErpDoc::StockInPost | ErpDoc::StockInCancel => stockin_bridge::internal_control_creator(biz_id).await,
            ErpDoc::StockTakePost => stocktake_bridge::internal_control_creator(biz_id).await,
            ErpDoc::PayablePay => payable_bridge::internal_control_creator(biz_id).await,
            ErpDoc::PayableCancel => payable_bridge::internal_control_creator_of_payable(biz_id).await,
            ErpDoc::VoucherReverse => voucher_bridge::internal_control_creator(biz_id).await,
            ErpDoc::ContractApprove => contract_bridge::internal_control_creator(biz_id).await,
            ErpDoc::BudgetApprove => budget_bridge::internal_control_creator(biz_id).await,
            ErpDoc::AssetDisposal => asset_bridge::internal_control_creator(biz_id).await,
            ErpDoc::Expense => expense_bridge::internal_control_creator(biz_id).await,
            ErpDoc::SoApprove => sales_approval_bridge::internal_control_creator(biz_id).await,
            ErpDoc::PrApprove => requisition_bridge::internal_control_creator(biz_id).await,
            ErpDoc::PrtApprove => purchase_return_bridge::internal_control_creator(biz_id).await,
            ErpDoc::CostSettle => cost_period_bridge::internal_control_creator(biz_id).await,
            ErpDoc::ReceiptRefund => receipt_refund_bridge::internal_control_creator(biz_id).await,
            // 调拨单不在受控集合里 → 本方法不会被调用（`of_biz` 先用 GOVERNED_CODES 过滤）。
            // 返回 `None` 而不是随便取一个 id：哪天有人把 STOCK_TRANSFER 加进受控集合，
            // 这里必须显式补上「制单人取哪一列」，不能靠一个静默的兜底值。
            ErpDoc::StockTransfer => Ok(None),
            ErpDoc::QuotationApprove => quotation_approval_bridge::internal_control_creator(biz_id).await,
            ErpDoc::PurchasePlan => purchase_plan_bridge::internal_control_creator(biz_id).await,
            ErpDoc::SupplierAccess => supplier_access_flow::internal_control_creator(biz_id).await,
            ErpDoc::FrameworkAgreement => framework_agreement_flow::internal_control_creator(biz_id).await,
            ErpDoc::CostStandardPublish => cost_standard_publish::internal_control_creator(biz_id).await,
        }
    }
}

/// 注册表里的一个 ERP 单据查询器（[`ErpDoc`] 的 trait 壳）。
#[derive(Debug, Clone, Copy)]
pub struct ErpDocReader {
    doc: ErpDoc,
}

impl ErpDocReader {
    /// 包一个实现。
    pub const fn new(doc: ErpDoc) -> Self {
        Self { doc }
    }

    /// 里面是哪个单据实现（测试 / 日志用）。
    pub const fn doc(&self) -> ErpDoc {
        self.doc
    }
}

#[async_trait]
impl DocReader for ErpDocReader {
    fn biz_type(&self) -> FlowBizType {
        self.doc.biz_type()
    }

    /// 核心方法：按单据 id 取快照（见 [`DocSpec::snapshot`]）。
    async fn doc_status(&self, doc_id: Uuid) -> DbResult<Option<DocSnapshot>> {
        self.doc.spec().snapshot(doc_id).await
    }

    /// 覆盖默认实现（默认从快照的 `creator_id` 派生）：走桥自己的查询，
    /// 这样「内控排除谁」与「审批详情显示谁制单」是**同一个来源**，
    /// 且与 P2-34 替换掉的直连调用逐字同源（见模块注释「三个方法都委派」）。
    async fn internal_control_creator(&self, biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
        self.doc.creator(biz_id).await
    }

    async fn on_settled(&self, instance_id: Option<Uuid>, instance_status: &str) {
        self.doc.settle(instance_id, instance_status).await;
    }

    async fn guard_self_approval(&self, task_id: Uuid, operator: Uuid) -> std::result::Result<(), ApiError> {
        self.doc.guard(task_id, operator).await
    }

    /// M3-11：审批点「同意」前的业务前置闸（只有销售订单覆盖，其余 `Ok(())`）。
    async fn guard_approve(&self, task_id: Uuid) -> std::result::Result<(), ApiError> {
        self.doc.guard_approve(task_id).await
    }
}

/// 全部 ERP 单据查询器（20 个，顺序 = [`ErpDoc::ALL`]）。
pub fn readers() -> Vec<Arc<dyn DocReader>> {
    ErpDoc::ALL
        .iter()
        .map(|doc| Arc::new(ErpDocReader::new(*doc)) as Arc<dyn DocReader>)
        .collect()
}

/// 把 18 个实现注册进**给定的**注册表（单测用；生产走 [`register_all`]）。
///
/// 返回注册成功的个数。重复注册（同一 `biz_type` 两个实现）返回 `Err` 且点名 ——
/// 不静默覆盖：两个实现抢一个键时「谁赢取决于注册顺序」，那种不确定性必须在启动期暴露。
pub fn register_into(registry: &mut DocRegistry) -> std::result::Result<usize, String> {
    let readers = readers();
    let count = readers.len();
    registry.register_all(readers)?;
    Ok(count)
}

/// 启动期注册到**进程级**注册表（[`crate::bootstrap`] 调用；请求期不许调）。
///
/// 返回注册个数（`Ok(18)`），失败时返回点名的 `Err`（重复注册 = 编程错误）。
pub fn register_all() -> std::result::Result<usize, String> {
    let readers = readers();
    let count = readers.len();
    // 进程级注册表由 `flow::doc_reader` 持有；这里不绕开它的写锁。
    doc_reader::register_all(readers)?;
    Ok(count)
}

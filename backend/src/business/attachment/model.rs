//! 单据附件数据层（`biz_doc_attachment`）。
//!
//! 只做三件事：**单据类型 → 表名映射 + 存在性校验**、**绑定关系读写**、**文件引用计数**。
//! 上传策略 / 秒传 / 对象写入 / 下载出流一律在 [`crate::business::storage`]，本文件不碰。
//!
//! # 五类单据的五张表
//!
//! | `doc_type` | 单据 | 表 | 前端详情页 |
//! |------------|------|----|-----------|
//! | `CONTRACT` | 合同 | `biz_contract` | `views/business/erp/contract` |
//! | `PURCHASE_ORDER` | 采购单 | `biz_purchase_order` | `views/business/erp/purchase` |
//! | `STOCK_IN` | 收货单 | `biz_stock_in` | `views/business/erp/stockin` |
//! | `PAYABLE` | 应付单 | `biz_payable` | `views/business/erp/payable` |
//! | `PAYMENT` | 付款单 | `biz_payment` | （应付详情里的付款记录） |
//!
//! 表名只来自本文件的常量白名单（[`DocKind::table`]），**不接受外部字符串拼 SQL** —— 这是
//! 「一张表存五类单据」这个设计的安全前提。
//!
//! # 为什么不建物理外键
//!
//! `doc_id` 要指向五张不同的表，单一列做不到。存在性由 [`doc_exists`] 在**写库之前**校验
//! （找不到 → 404，不会写出悬空附件）；`file_id` 反而**有**外键（同一张 `sys_file`），
//! 且是 `ON DELETE RESTRICT`：文件记录都不在了，绑定行就没有意义。
//!
//! # SCOPE-EXEMPT: 附件的可见性 = 单据的可见性
//!
//! 本文件的查询不注入 `data_scope`：七张单据表都没有部门列 / 归属人列，数据范围
//! 在它们上面表达不出来（各 ERP 模块文件头同一条 `SCOPE-EXEMPT`）。
//!
//! **ATT-02 起，这层「单据可见性」由 handler 显式校验**，而不是默默假设：
//! [`DocKind::view_permissions`] 给出各类单据的模块读码，`handler::list` /
//! `handler::download` 在取数据之前先比一次。此前只校验了路由码
//! `business:attachment:list`（= 「能用附件功能」），于是**任何**持该码的人都能
//! 拿别人的应付单 / 报销单附件（同一台机器上仓库主管读财务凭证影像就是一次越权）。
//!
//! 与 `biz_task_attachment`（`task/collab.rs` 文件头同款豁免）的差别：任务附件的
//! 单条读路径会先 `find_detail(&principal, task_id)`（看不见即 404），本模块此前没有
//! 等价的一步 —— ATT-02 补的就是它。

use nik::prelude::*;
use nik::uuid::Uuid;

/// 单据类型：合同。
pub const DOC_CONTRACT: &str = "CONTRACT";
/// 单据类型：采购订单。
pub const DOC_PURCHASE_ORDER: &str = "PURCHASE_ORDER";
/// 单据类型：收货入库单。
pub const DOC_STOCK_IN: &str = "STOCK_IN";
/// 单据类型：应付单。
pub const DOC_PAYABLE: &str = "PAYABLE";
/// 单据类型：付款登记单。
pub const DOC_PAYMENT: &str = "PAYMENT";
/// 单据类型：固定资产卡片（FIN-2：发票 / 验收单 / 处置单）。
pub const DOC_FIXED_ASSET: &str = "FIXED_ASSET";
/// 单据类型：费用报销单（FIN-3：发票影像）。
pub const DOC_EXPENSE_REPORT: &str = "EXPENSE_REPORT";
/// 单据类型：销售发票（ATT-06：发票影像必传）。
pub const DOC_INVOICE: &str = "INVOICE";
/// 单据类型：收款单（ATT-06：银行回单必传）。
pub const DOC_RECEIPT: &str = "RECEIPT";
/// 单据类型：盘点单（ATT-06：确认时盘点表必传）。
pub const DOC_STOCK_TAKE: &str = "STOCK_TAKE";
/// 单据类型：采购退货单（ATT-06：提交时退货凭证必传）。
pub const DOC_PURCHASE_RETURN: &str = "PURCHASE_RETURN";
/// 单据类型：销售退货单（ATT-06：提交时退货凭证必传）。
pub const DOC_SALES_RETURN: &str = "SALES_RETURN";
/// 单据类型：库存调拨单（ATT-11：建议上传调拨审批单）。
pub const DOC_STOCK_TRANSFER: &str = "STOCK_TRANSFER";
/// 单据类型：供应商（主数据，ATT-11：营业执照 / 开户许可 / 资质证书）。
pub const DOC_SUPPLIER: &str = "SUPPLIER";
/// 单据类型：客户（主数据，ATT-11：营业执照 / 信用资料）。
pub const DOC_CUSTOMER: &str = "CUSTOMER";
/// 单据类型：期初余额（ATT-11：期初依据；`doc_id` = 期初凭证 id）。
pub const DOC_OPENING_BALANCE: &str = "OPENING_BALANCE";

/// 文件角色：合同扫描件。
pub const ROLE_CONTRACT_SCAN: &str = "CONTRACT_SCAN";
/// 文件角色：补充协议（ATT-04）。
pub const ROLE_SUPPLEMENT: &str = "SUPPLEMENT";
/// 文件角色：验收单（ATT-04）。
pub const ROLE_ACCEPTANCE: &str = "ACCEPTANCE";
/// 文件角色：发票影像（ATT-04 规范值）。
pub const ROLE_INVOICE_SCAN: &str = "INVOICE_SCAN";
/// 文件角色：银行回单（ATT-04）。
pub const ROLE_BANK_RECEIPT: &str = "BANK_RECEIPT";
/// 文件角色：签收单（V0212 老值，保留兼容，不再作为上传选项）。
pub const ROLE_SIGN_RECEIPT: &str = "SIGN_RECEIPT";
/// 文件角色：其他附件（ATT-04 规范值 = 缺省）。
pub const ROLE_DEFAULT: &str = "DEFAULT";
/// 文件角色老值：发票影像（V0212 拼法，解析时归一为 `INVOICE_SCAN`）。
pub const ROLE_INVOICE: &str = "INVOICE";
/// 文件角色老值：其他（V0212 拼法，解析时归一为 `DEFAULT`）。
pub const ROLE_OTHER: &str = "OTHER";
/// 文件角色：营业执照（ATT-11，供应商 / 客户）。
pub const ROLE_BIZ_LICENSE: &str = "BIZ_LICENSE";
/// 文件角色：开户许可（ATT-11，供应商）。
pub const ROLE_BANK_ACCOUNT_PERMIT: &str = "BANK_ACCOUNT_PERMIT";
/// 文件角色：资质证书（ATT-11，供应商）。
pub const ROLE_QUALIFICATION: &str = "QUALIFICATION";
/// 文件角色：信用资料（ATT-11，客户）。
pub const ROLE_CREDIT_FILE: &str = "CREDIT_FILE";
/// 文件角色：报价单（ATT-11，采购订单）。
pub const ROLE_QUOTATION: &str = "QUOTATION";
/// 文件角色：客户 PO（ATT-11，采购订单 —— 客户下的采购单原件）。
pub const ROLE_CUSTOMER_PO: &str = "CUSTOMER_PO";
/// 文件角色：调拨审批单（ATT-11，库存调拨单）。
pub const ROLE_TRANSFER_APPROVAL: &str = "TRANSFER_APPROVAL";
/// 文件角色：期初依据（ATT-11，期初余额）。
pub const ROLE_OPENING_EVIDENCE: &str = "OPENING_EVIDENCE";

/// 单张单据的附件上限（**读流之前**判定，避免大文件先读进内存再被拒）。
///
/// 取 50 与任务附件同口径：扫描件按「一份单据 3–10 张」估算，50 足够；上限的意义是
/// 防止误拖整个文件夹把单据变成图床。
pub const MAX_ATTACHMENTS_PER_DOC: i64 = 50;

/// 列表排序口径（ATT-03）。
///
/// # 为什么是一个枚举而不是把 ORDER BY 字符串透传到 SQL
///
/// `ORDER BY` 拼外部字符串是注入面；本枚举是**唯一来源**（[`ListOrder::order_by`]
/// 里的常量 SQL），入参只能落到两个分支之一，未知值在 [`ListOrder::parse`] 就返回 `None`。
///
/// # 两种口径
///
/// | 口径 | 顺序 | 用途 |
/// |------|------|------|
/// | [`ListOrder::TimeDesc`] | `created_at DESC` | **默认**：刚传的在上（与任务附件 / 文件管理同口径） |
/// | [`ListOrder::Manual`] | `sort_order ASC` → `created_at DESC` | 用户手动排过序（上移 / 下移） |
///
/// 两种都以 `a.id DESC` 收尾：同秒上传的多个文件不会因刷新而换位置（UUID v7 时间有序，
/// 作最终 tiebreak 稳定且与业务无关）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListOrder {
    /// 按上传时间倒序（默认）。
    TimeDesc,
    /// 按 `sort_order` 升序（手动排序），同序号再按上传时间倒序。
    Manual,
}

impl ListOrder {
    /// 入参字面值（`GET /attachment/list?sort=`）。
    pub fn as_str(self) -> &'static str {
        match self {
            ListOrder::TimeDesc => "time",
            ListOrder::Manual => "manual",
        }
    }

    /// 解析排序口径：**缺省 / 空串 → 时间倒序**（老前端不带 `sort` 也能用）。
    ///
    /// # 参数
    ///
    /// - `raw`：请求里的 `sort`（大小写不敏感、去空白）。
    ///
    /// # 返回值
    ///
    /// 合法口径；未知值返回 `None`（调用方报 400 并列出可选值，别静默回落成默认 ——
    /// 「写错参数却拿到结果」会让人以为排序生效了）。
    pub fn parse(raw: Option<&str>) -> Option<ListOrder> {
        let Some(text) = raw
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_ascii_lowercase)
        else {
            return Some(ListOrder::TimeDesc);
        };
        match text.as_str() {
            "time" | "time_desc" | "created_at" => Some(ListOrder::TimeDesc),
            "manual" | "sort" | "sort_order" => Some(ListOrder::Manual),
            _ => None,
        }
    }

    /// `ORDER BY` 子句（常量 SQL，唯一来源：改排序口径只改这里）。
    pub fn order_by(self) -> &'static str {
        match self {
            ListOrder::TimeDesc => "ORDER BY a.created_at DESC, a.id DESC",
            ListOrder::Manual => "ORDER BY a.sort_order ASC, a.created_at DESC, a.id DESC",
        }
    }
}

/// 单据类型枚举（与迁移里的 `ck_biz_doc_attachment_type` 逐字对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    /// 合同 → `biz_contract`
    Contract,
    /// 采购订单 → `biz_purchase_order`
    PurchaseOrder,
    /// 收货入库单 → `biz_stock_in`
    StockIn,
    /// 应付单 → `biz_payable`
    Payable,
    /// 付款登记 → `biz_payment`
    Payment,
    /// 固定资产卡片 → `biz_fixed_asset`（ARCH-6 §6.2.4：发票 / 验收单 / 处置单）
    FixedAsset,
    /// 费用报销单 → `biz_expense_report`（ARCH-6 §4.7.1：发票影像；明细级附件 v1 不做）
    ExpenseReport,
    /// 销售发票 → `biz_invoice`（ATT-06：发票影像必传，电子 / 纸质按 `invoice_code` 分支）
    Invoice,
    /// 收款单 → `biz_receipt`（ATT-06：银行回单必传）
    Receipt,
    /// 盘点单 → `biz_stock_take`（ATT-06：确认时盘点表必传）
    StockTake,
    /// 采购退货单 → `biz_purchase_return`（ATT-06：提交时退货凭证必传）
    PurchaseReturn,
    /// 销售退货单 → `biz_sales_return`（ATT-06：提交时退货凭证必传）
    SalesReturn,
    /// 库存调拨单 → `biz_stock_transfer`（ATT-11：建议上传调拨审批单）
    StockTransfer,
    /// 供应商 → `biz_supplier`（ATT-11：主数据，营业执照 / 开户许可 / 资质证书）
    Supplier,
    /// 客户 → `biz_customer`（ATT-11：主数据，营业执照 / 信用资料）
    Customer,
    /// 期初余额 → `biz_voucher`（ATT-11：期初依据；`doc_id` = 期初凭证 id）
    ///
    /// # 为什么存在性校验查的是凭证
    ///
    /// 期初是「按期间整体覆盖」的录入动作（`biz_opening_balance` 一个科目一行、
    /// 每次保存整期覆盖），没有单头行可以当 `doc_id`。一个期间只有**一张**期初凭证
    /// （`biz_voucher.voucher_type='OPENING'`，V0189 的唯一索引兜死），
    /// 它才是「这份依据属于哪一期期初」的稳定锚点。
    OpeningBalance,
}

impl DocKind {
    /// 全部类型（顺序与前端下拉一致）。
    ///
    /// ATT-06 从 7 类扩到 12 类：新增 `INVOICE` / `RECEIPT` / `STOCK_TAKE` /
    /// `PURCHASE_RETURN` / `SALES_RETURN`，与 `V0347` 放宽后的
    /// `ck_biz_doc_attachment_type` 逐字对应。
    pub const ALL: [DocKind; 16] = [
        DocKind::Contract,
        DocKind::PurchaseOrder,
        DocKind::StockIn,
        DocKind::Payable,
        DocKind::Payment,
        DocKind::FixedAsset,
        DocKind::ExpenseReport,
        DocKind::Invoice,
        DocKind::Receipt,
        DocKind::StockTake,
        DocKind::PurchaseReturn,
        DocKind::SalesReturn,
        // ATT-11：建议传（不校验）的 4 类，V0350 把 CHECK 放宽到 16 值。
        DocKind::StockTransfer,
        DocKind::Supplier,
        DocKind::Customer,
        DocKind::OpeningBalance,
    ];

    /// 解析单据类型（大小写不敏感、去空白）。
    ///
    /// # 参数
    ///
    /// - `raw`：请求里的 `doc_type`。
    ///
    /// # 返回值
    ///
    /// 合法类型；未知类型返回 `None`（调用方报 400 + `PARAM_INVALID` 并列出可选值）。
    pub fn parse(raw: &str) -> Option<DocKind> {
        match raw.trim().to_ascii_uppercase().as_str() {
            DOC_CONTRACT => Some(DocKind::Contract),
            DOC_PURCHASE_ORDER => Some(DocKind::PurchaseOrder),
            DOC_STOCK_IN => Some(DocKind::StockIn),
            DOC_PAYABLE => Some(DocKind::Payable),
            DOC_PAYMENT => Some(DocKind::Payment),
            DOC_FIXED_ASSET => Some(DocKind::FixedAsset),
            DOC_EXPENSE_REPORT => Some(DocKind::ExpenseReport),
            DOC_INVOICE => Some(DocKind::Invoice),
            DOC_RECEIPT => Some(DocKind::Receipt),
            DOC_STOCK_TAKE => Some(DocKind::StockTake),
            DOC_PURCHASE_RETURN => Some(DocKind::PurchaseReturn),
            DOC_SALES_RETURN => Some(DocKind::SalesReturn),
            DOC_STOCK_TRANSFER => Some(DocKind::StockTransfer),
            DOC_SUPPLIER => Some(DocKind::Supplier),
            DOC_CUSTOMER => Some(DocKind::Customer),
            DOC_OPENING_BALANCE => Some(DocKind::OpeningBalance),
            _ => None,
        }
    }

    /// 数据库 / 前端使用的字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            DocKind::Contract => DOC_CONTRACT,
            DocKind::PurchaseOrder => DOC_PURCHASE_ORDER,
            DocKind::StockIn => DOC_STOCK_IN,
            DocKind::Payable => DOC_PAYABLE,
            DocKind::Payment => DOC_PAYMENT,
            DocKind::FixedAsset => DOC_FIXED_ASSET,
            DocKind::ExpenseReport => DOC_EXPENSE_REPORT,
            DocKind::Invoice => DOC_INVOICE,
            DocKind::Receipt => DOC_RECEIPT,
            DocKind::StockTake => DOC_STOCK_TAKE,
            DocKind::PurchaseReturn => DOC_PURCHASE_RETURN,
            DocKind::SalesReturn => DOC_SALES_RETURN,
            DocKind::StockTransfer => DOC_STOCK_TRANSFER,
            DocKind::Supplier => DOC_SUPPLIER,
            DocKind::Customer => DOC_CUSTOMER,
            DocKind::OpeningBalance => DOC_OPENING_BALANCE,
        }
    }

    /// 中文标签（错误消息与审计用）。
    pub fn label(self) -> &'static str {
        match self {
            DocKind::Contract => "合同",
            DocKind::PurchaseOrder => "采购单",
            DocKind::StockIn => "收货单",
            DocKind::Payable => "应付单",
            DocKind::Payment => "付款单",
            DocKind::FixedAsset => "固定资产",
            DocKind::ExpenseReport => "费用报销单",
            DocKind::Invoice => "发票",
            DocKind::Receipt => "收款单",
            DocKind::StockTake => "盘点单",
            DocKind::PurchaseReturn => "采购退货单",
            DocKind::SalesReturn => "销售退货单",
            DocKind::StockTransfer => "库存调拨单",
            DocKind::Supplier => "供应商",
            DocKind::Customer => "客户",
            DocKind::OpeningBalance => "期初余额",
        }
    }

    /// 单据所在表（**唯一白名单**，禁止用外部字符串拼 SQL）。
    pub fn table(self) -> &'static str {
        match self {
            DocKind::Contract => "biz_contract",
            DocKind::PurchaseOrder => "biz_purchase_order",
            DocKind::StockIn => "biz_stock_in",
            DocKind::Payable => "biz_payable",
            DocKind::Payment => "biz_payment",
            DocKind::FixedAsset => "biz_fixed_asset",
            DocKind::ExpenseReport => "biz_expense_report",
            DocKind::Invoice => "biz_invoice",
            DocKind::Receipt => "biz_receipt",
            DocKind::StockTake => "biz_stock_take",
            DocKind::PurchaseReturn => "biz_purchase_return",
            DocKind::SalesReturn => "biz_sales_return",
            DocKind::StockTransfer => "biz_stock_transfer",
            DocKind::Supplier => "biz_supplier",
            DocKind::Customer => "biz_customer",
            // 期初余额没有单头表：锚点是期初凭证（`voucher_type = 'OPENING'`）
            DocKind::OpeningBalance => "biz_voucher",
        }
    }

    /// 单据存在性 SQL（`deleted_at IS NULL` 都成立：五张表都是软删模型）。
    pub fn exists_sql(self) -> &'static str {
        match self {
            DocKind::Contract => {
                "SELECT 1 FROM biz_contract WHERE id = ?::uuid AND deleted_at IS NULL"
            }
            DocKind::PurchaseOrder => {
                "SELECT 1 FROM biz_purchase_order WHERE id = ?::uuid AND deleted_at IS NULL"
            }
            DocKind::StockIn => {
                "SELECT 1 FROM biz_stock_in WHERE id = ?::uuid AND deleted_at IS NULL"
            }
            DocKind::Payable => {
                "SELECT 1 FROM biz_payable WHERE id = ?::uuid AND deleted_at IS NULL"
            }
            DocKind::Payment => {
                "SELECT 1 FROM biz_payment WHERE id = ?::uuid AND deleted_at IS NULL"
            }
            // FIN-2：资产（发票 / 验收单 / 处置单）。它与 DDL 的
            // `ck_biz_doc_attachment_type`（V0232 放宽到 6 值）逐字对应。
            DocKind::FixedAsset => {
                "SELECT 1 FROM biz_fixed_asset WHERE id = ?::uuid AND deleted_at IS NULL"
            }
            // FIN-3：报销单（发票影像）。与 V0244 放宽后的
            // `ck_biz_doc_attachment_type`（7 值）逐字对应。
            DocKind::ExpenseReport => {
                "SELECT 1 FROM biz_expense_report WHERE id = ?::uuid AND deleted_at IS NULL"
            }
            // ATT-06：发票 / 收款 / 盘点 / 采购退货 / 销售退货（迁移 V0347 放宽到 12 值）
            DocKind::Invoice => {
                "SELECT 1 FROM biz_invoice WHERE id = ?::uuid AND deleted_at IS NULL"
            }
            DocKind::Receipt => {
                "SELECT 1 FROM biz_receipt WHERE id = ?::uuid AND deleted_at IS NULL"
            }
            DocKind::StockTake => {
                "SELECT 1 FROM biz_stock_take WHERE id = ?::uuid AND deleted_at IS NULL"
            }
            DocKind::PurchaseReturn => {
                "SELECT 1 FROM biz_purchase_return WHERE id = ?::uuid AND deleted_at IS NULL"
            }
            DocKind::SalesReturn => {
                "SELECT 1 FROM biz_sales_return WHERE id = ?::uuid AND deleted_at IS NULL"
            }
            // ATT-11：调拨单 / 主数据（供应商、客户）
            DocKind::StockTransfer => {
                "SELECT 1 FROM biz_stock_transfer WHERE id = ?::uuid AND deleted_at IS NULL"
            }
            DocKind::Supplier => {
                "SELECT 1 FROM biz_supplier WHERE id = ?::uuid AND deleted_at IS NULL"
            }
            DocKind::Customer => {
                "SELECT 1 FROM biz_customer WHERE id = ?::uuid AND deleted_at IS NULL"
            }
            // ATT-11：期初余额的存在性 = 「这一期已经有期初凭证」。
            // 只认 `voucher_type = 'OPENING'`：传一张普通凭证的 id 不算期初单据
            // （否则附件会挂到任意凭证上，与「期初依据」的语义无关）。
            DocKind::OpeningBalance => {
                "SELECT 1 FROM biz_voucher \
                  WHERE id = ?::uuid AND voucher_type = 'OPENING' AND deleted_at IS NULL"
            }
        }
    }

    /// 查看这一类单据所需的权限码（**ATT-02**：附件可见性 = 单据可见性）。
    ///
    /// # 这条判据从哪来
    ///
    /// 单据的可见性在本仓是**由模块权限码控制**的，不是 `data_scope`：
    /// 七张单据表都没有部门列 / 归属人列，`data_scope`（SELF / DEPT_*）在它们上面
    /// 表达不出来 —— 各模块文件头都写了同一条 `SCOPE-EXEMPT`（例：
    /// `erp/payable.rs`「应付单是财务台账（V0148 建表，无 `dept_id` / 归属人列），
    /// 数据范围表达不出来；可见性由 `business:erp:payable:*` 权限码控制」）。
    /// 附件若另立一套判据，就会出现「列表看得见单据、详情页却打不开它的附件」——
    /// 那种不一致比少一层过滤更容易变成长期隐患。
    ///
    /// 所以这里**照抄各模块自己的读码**：列表码 ∪（存在的）详情码。
    /// 只给「能进入该模块并看到这张单的人」发附件。
    ///
    /// # 为什么是切片而不是单个码
    ///
    /// 有的模块详情与列表同码（合同 / 采购单：`GET {id}` 也挂 `:list`），有的模块
    /// 另有详情码（应付 / 收货 / 资产 / 报销）。给一个**或**关系，才不会让「只有详情码」
    /// 的角色被误挡。
    ///
    /// # 与库的一致性
    ///
    /// 每个字面值都必须在 `sys_menu.auth_code` 里真实存在（否则就是「点了必然 403」
    /// 的哑弹）。`business:erp:purchase-order:detail` 曾在 `V0146` 种过、随后按
    /// BUG-SYS-05 删除（采购订单没有 GET 详情路由），所以它**不在**下面的清单里。
    /// `tests::view_permissions_are_real_menu_codes`（真库用例）逐个回查库。
    pub fn view_permissions(self) -> &'static [&'static str] {
        match self {
            DocKind::Contract => &["business:erp:contract:list"],
            DocKind::PurchaseOrder => &["business:erp:purchase-order:list"],
            DocKind::StockIn => &["business:erp:stock-in:list", "business:erp:stock-in:detail"],
            // 付款单是「应付单的付款记录」（没有独立的付款菜单），可见性同应付。
            DocKind::Payable | DocKind::Payment => {
                &["business:erp:payable:list", "business:erp:payable:detail"]
            }
            DocKind::FixedAsset => &["business:erp:asset:list", "business:erp:asset:detail"],
            DocKind::ExpenseReport => &["business:erp:expense:list", "business:erp:expense:detail"],
            DocKind::Invoice => &["business:erp:invoice:list", "business:erp:invoice:detail"],
            DocKind::Receipt => &["business:erp:receipt:list", "business:erp:receipt:detail"],
            DocKind::StockTake => &[
                "business:erp:stock-take:list",
                "business:erp:stock-take:detail",
            ],
            // 采购退货 / 销售退货没有独立的 GET 详情路由（`GET {id}` 也挂 `:list`），
            // 与采购订单同口径：只给列表码（多给一个不存在的码就是「点了必然 403」的哑弹）。
            DocKind::PurchaseReturn => &["business:erp:purchase-return:list"],
            DocKind::SalesReturn => &["business:erp:sales-return:list"],
            // ATT-11：调拨单（列表 / 详情两码）、主数据（列表码）、期初（读码）。
            // 与各模块自己的读码逐字一致；`view_permissions_are_real_menu_codes` 真库回查。
            DocKind::StockTransfer => {
                &["business:erp:transfer:list", "business:erp:transfer:detail"]
            }
            DocKind::Supplier => &["business:erp:supplier:list"],
            DocKind::Customer => &["business:erp:customer:list"],
            DocKind::OpeningBalance => &["business:erp:opening:read"],
        }
    }
}

/// 文件角色枚举（与迁移里的 `ck_biz_doc_attachment_role` 逐字对应）。
///
/// # 两套词表：`GROUPS`（ATT-04 分栏）vs `ALL`（可写入范围）
///
/// ATT-04 要求附件面板按 `file_role` 分栏，卡面给的是 6 个角色（[`FileRole::GROUPS`]）。
/// V0212 建的 CHECK 还有两个老值：`SIGN_RECEIPT`（签收单，真实业务语义，保留）与
/// `INVOICE` / `OTHER`（与 `INVOICE_SCAN` / `DEFAULT` 同义，**解析时归一**）。
///
/// | 常量 | 用途 |
/// |------|------|
/// | [`FileRole::GROUPS`] | 面板「一定要出现」的分组（空分组也要显示上传引导） |
/// | [`FileRole::ALL`] | 可被解析 / 可被落库的全部值（含老值 `SIGN_RECEIPT`） |
///
/// 为什么 `SIGN_RECEIPT` 不进 `GROUPS`：收货签收 ≠ 合同验收，不能并进 `ACCEPTANCE`；
/// 但它没有对应的新值，所以保留为**兼容值** —— 老数据照常显示（`dto::role_catalog`
/// 会在有数据时把它追加成一个分组），新上传不再提供这个选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileRole {
    /// 合同扫描件
    ContractScan,
    /// 补充协议
    Supplement,
    /// 验收单
    Acceptance,
    /// 发票影像
    InvoiceScan,
    /// 银行回单
    BankReceipt,
    /// 签收单（V0212 老值，仅兼容：有数据时显示，不再作为上传选项）
    SignReceipt,
    /// 营业执照（ATT-11：供应商 / 客户）
    BizLicense,
    /// 开户许可（ATT-11：供应商）
    BankAccountPermit,
    /// 资质证书（ATT-11：供应商）
    Qualification,
    /// 信用资料（ATT-11：客户）
    CreditFile,
    /// 报价单（ATT-11：采购订单）
    Quotation,
    /// 客户 PO（ATT-11：采购订单）
    CustomerPo,
    /// 调拨审批单（ATT-11：库存调拨单）
    TransferApproval,
    /// 期初依据（ATT-11：期初余额）
    OpeningEvidence,
    /// 其他附件（缺省）
    Default,
}

impl FileRole {
    /// 面板分栏的全部角色（顺序 = 前端上传下拉 / 分组顺序）。
    ///
    /// 空分组也要显示（「暂无 XX，点击上传」是卡面验收②），所以这个清单是**固定**的，
    /// 不能由「库里有哪些值」推出来。
    ///
    /// # ATT-11：从 6 个扩到 14 个，但每张单据只画自己那几个
    ///
    /// 卡面要求按业务给角色（供应商 = 营业执照 / 开户许可 / 资质证书…），
    /// 若把 14 个分组都画到每张单据上，一张合同详情下面会挂 13 个空分组 —— 那是噪声不是信息。
    /// 所以本常量是**全量词表**，真正渲染哪几个由 [`FileRole::catalog`] 按单据类型决定。
    pub const GROUPS: [FileRole; 14] = [
        FileRole::ContractScan,
        FileRole::Supplement,
        FileRole::Acceptance,
        FileRole::InvoiceScan,
        FileRole::BankReceipt,
        FileRole::Quotation,
        FileRole::CustomerPo,
        FileRole::BizLicense,
        FileRole::BankAccountPermit,
        FileRole::Qualification,
        FileRole::CreditFile,
        FileRole::TransferApproval,
        FileRole::OpeningEvidence,
        FileRole::Default,
    ];

    /// 全部可解析 / 可落库的角色（`GROUPS` + 兼容值 `SIGN_RECEIPT`）。
    pub const ALL: [FileRole; 15] = [
        FileRole::ContractScan,
        FileRole::Supplement,
        FileRole::Acceptance,
        FileRole::InvoiceScan,
        FileRole::BankReceipt,
        FileRole::SignReceipt,
        FileRole::BizLicense,
        FileRole::BankAccountPermit,
        FileRole::Qualification,
        FileRole::CreditFile,
        FileRole::Quotation,
        FileRole::CustomerPo,
        FileRole::TransferApproval,
        FileRole::OpeningEvidence,
        FileRole::Default,
    ];

    /// 某类单据在面板上要画的角色分组（顺序 = 渲染顺序，`DEFAULT` 永远最后）。
    ///
    /// # 这是「建议」不是「白名单」
    ///
    /// 上传时后端**不校验** role 与单据类型的匹配（`file_role` 是分类不是契约），
    /// 分组列表的用处是「告诉用户通常该传什么 + 让空分组成为上传入口」。
    /// 已有数据里出现了不在本表里的角色（老数据 / 接口直传），
    /// [`super::dto::role_catalog`] 会按 `ALL` 顺序补一个分组出来 —— 附件绝不凭空消失。
    pub const fn catalog(kind: DocKind) -> &'static [FileRole] {
        use FileRole::{
            Acceptance, BankAccountPermit, BankReceipt, BizLicense, ContractScan, CreditFile,
            CustomerPo, Default, InvoiceScan, OpeningEvidence, Qualification, Quotation,
            Supplement, TransferApproval,
        };
        match kind {
            // 合同：扫描件 + 补充协议（ATT-11 明确补充协议要独立分组）
            DocKind::Contract => &[ContractScan, Supplement, Default],
            // 采购：报价单 / 客户 PO
            DocKind::PurchaseOrder => &[Quotation, CustomerPo, Default],
            DocKind::StockIn => &[Acceptance, Default],
            DocKind::Payable | DocKind::Payment => &[InvoiceScan, BankReceipt, Default],
            DocKind::FixedAsset => &[InvoiceScan, Acceptance, Default],
            DocKind::ExpenseReport | DocKind::Invoice => &[InvoiceScan, Default],
            DocKind::Receipt => &[BankReceipt, Default],
            // 盘点表 / 盈亏说明 / 退货凭证：没有专用角色，统一进「其他附件」
            DocKind::StockTake | DocKind::PurchaseReturn | DocKind::SalesReturn => &[Default],
            // ATT-11
            DocKind::StockTransfer => &[TransferApproval, Default],
            DocKind::OpeningBalance => &[OpeningEvidence, Default],
            DocKind::Supplier => &[BizLicense, BankAccountPermit, Qualification, Default],
            DocKind::Customer => &[BizLicense, CreditFile, Default],
        }
    }

    /// 解析角色：**空值回落 `DEFAULT`**，未知值返回 `None`。
    ///
    /// 空值回落是刻意的：`file_role` 是分类而非契约，前端老版本不带该字段时
    /// 不应该把上传挡在 400（卡面验收④：不传 role 的文件进 DEFAULT 组）。
    ///
    /// # 老值归一（ATT-04）
    ///
    /// | 入参 | 结果 | 理由 |
    /// |------|------|------|
    /// | `INVOICE` | `INVOICE_SCAN` | 同一事物的两种拼法，不归一会显示成两个「发票影像」分组 |
    /// | `OTHER` | `DEFAULT` | 同上 |
    /// | `SIGN_RECEIPT` | `SIGN_RECEIPT` | 没有等价新值，原样保留（兼容老前端 / 老数据） |
    pub fn parse(raw: Option<&str>) -> Option<FileRole> {
        let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
            return Some(FileRole::Default);
        };
        match text.to_ascii_uppercase().as_str() {
            ROLE_CONTRACT_SCAN => Some(FileRole::ContractScan),
            ROLE_SUPPLEMENT => Some(FileRole::Supplement),
            ROLE_ACCEPTANCE => Some(FileRole::Acceptance),
            ROLE_INVOICE_SCAN => Some(FileRole::InvoiceScan),
            // 老值归一（见上表）
            ROLE_INVOICE => Some(FileRole::InvoiceScan),
            ROLE_BANK_RECEIPT => Some(FileRole::BankReceipt),
            ROLE_SIGN_RECEIPT => Some(FileRole::SignReceipt),
            ROLE_BIZ_LICENSE => Some(FileRole::BizLicense),
            ROLE_BANK_ACCOUNT_PERMIT => Some(FileRole::BankAccountPermit),
            ROLE_QUALIFICATION => Some(FileRole::Qualification),
            ROLE_CREDIT_FILE => Some(FileRole::CreditFile),
            ROLE_QUOTATION => Some(FileRole::Quotation),
            ROLE_CUSTOMER_PO => Some(FileRole::CustomerPo),
            ROLE_TRANSFER_APPROVAL => Some(FileRole::TransferApproval),
            ROLE_OPENING_EVIDENCE => Some(FileRole::OpeningEvidence),
            ROLE_DEFAULT => Some(FileRole::Default),
            ROLE_OTHER => Some(FileRole::Default),
            _ => None,
        }
    }

    /// 数据库 / 前端使用的字面值（**归一后的规范值**）。
    pub fn as_str(self) -> &'static str {
        match self {
            FileRole::ContractScan => ROLE_CONTRACT_SCAN,
            FileRole::Supplement => ROLE_SUPPLEMENT,
            FileRole::Acceptance => ROLE_ACCEPTANCE,
            FileRole::InvoiceScan => ROLE_INVOICE_SCAN,
            FileRole::BankReceipt => ROLE_BANK_RECEIPT,
            FileRole::SignReceipt => ROLE_SIGN_RECEIPT,
            FileRole::BizLicense => ROLE_BIZ_LICENSE,
            FileRole::BankAccountPermit => ROLE_BANK_ACCOUNT_PERMIT,
            FileRole::Qualification => ROLE_QUALIFICATION,
            FileRole::CreditFile => ROLE_CREDIT_FILE,
            FileRole::Quotation => ROLE_QUOTATION,
            FileRole::CustomerPo => ROLE_CUSTOMER_PO,
            FileRole::TransferApproval => ROLE_TRANSFER_APPROVAL,
            FileRole::OpeningEvidence => ROLE_OPENING_EVIDENCE,
            FileRole::Default => ROLE_DEFAULT,
        }
    }

    /// 中文标签（列表里的 tag / 面板分组标题）。
    ///
    /// 与卡面（ATT-04）的词表逐字一致；前端 `FILE_ROLE_OPTIONS` 与本文案由
    /// `tests::file_roles_match_frontend_vocabulary` 逐条比对（漂了直接红）。
    pub fn label(self) -> &'static str {
        match self {
            FileRole::ContractScan => "合同扫描件",
            FileRole::Supplement => "补充协议",
            FileRole::Acceptance => "验收单",
            FileRole::InvoiceScan => "发票影像",
            FileRole::BankReceipt => "银行回单",
            FileRole::SignReceipt => "签收单",
            FileRole::BizLicense => "营业执照",
            FileRole::BankAccountPermit => "开户许可",
            FileRole::Qualification => "资质证书",
            FileRole::CreditFile => "信用资料",
            FileRole::Quotation => "报价单",
            FileRole::CustomerPo => "客户PO",
            FileRole::TransferApproval => "调拨审批单",
            FileRole::OpeningEvidence => "期初依据",
            FileRole::Default => "其他附件",
        }
    }
}

/// 附件行（列表 / 详情共用）。
#[derive(Debug, Clone)]
pub struct DocAttachmentRow {
    /// 绑定行主键（删除 / 下载用的 `{id}`）。
    pub id: Uuid,
    /// 单据类型。
    pub doc_type: String,
    /// 单据主键。
    pub doc_id: Uuid,
    /// `sys_file.id`。
    pub file_id: Uuid,
    /// 文件角色。
    pub file_role: String,
    /// 备注。
    pub remark: Option<String>,
    /// 上传人。
    pub uploaded_by: Option<Uuid>,
    /// 上传人姓名（`COALESCE(real_name, username, '')`）。
    pub uploader_name: String,
    /// 显示用文件名（`original_name` 优先，回落对象键）。
    pub file_name: String,
    /// 字节数。
    pub file_size: i64,
    /// MIME。
    pub mime_type: Option<String>,
    /// 手动排序序号（ATT-03：`sort_order ASC` 是手动模式的排序键）。
    pub sort_order: i32,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// 列表 / 详情要读的列（唯一来源，改列名只改这里）。
///
/// **`original_name` 与 `file_name` 分开取原始值**：显示名由 [`display_name`] 算。
/// 若直接 `COALESCE(original_name, file_name)`，存储路径（`2026/11/03/….png`）
/// 就会漏进前端当下载保存名（任务附件 E12 踩过同一个坑）。
///
/// # `uploader_name` 的三段回落（AUD-01）
///
/// ```text
///   ① a.uploader_name            上传时落库的姓名快照 —— 优先（改名后仍显示当时的名字）
///   ② u.real_name / u.username   实时 join —— 只给 V0352 之前的历史行兜底（回填未跑）
///   ③ ''                          两者都没有 → 空串，前端显示 '-'（**永不回落 UUID**）
/// ```
///
/// 为什么快照优先而不是现值优先：附件上传人是**审计属性**（AGENTS.md 约定 13）。
/// 用户 3 月改名，6 月回看 1 月的合同扫描件，必须显示 3 月那时的名字。
const COLUMNS: &str = "a.id, a.doc_type, a.doc_id, a.file_id, a.file_role, a.remark, \
                       a.uploaded_by, \
                       COALESCE(NULLIF(a.uploader_name, ''), NULLIF(u.real_name, ''), u.username, '') AS uploader_name, \
                       COALESCE(f.original_name, '') AS original_name, \
                       COALESCE(f.file_name, '') AS stored_name, \
                       COALESCE(f.file_size, 0) AS file_size, f.mime_type, a.sort_order, a.created_at";

/// 显示用文件名：原始名优先，为空时退回对象键的 basename（不含日期目录）。
///
/// # 参数
///
/// - `original`：`sys_file.original_name`；
/// - `stored`：`sys_file.file_name`（对象键，形如 `2026/11/03/<uuid>.png`）。
pub fn display_name(original: Option<&str>, stored: &str) -> String {
    if let Some(name) = original.map(str::trim).filter(|name| !name.is_empty()) {
        return name.to_string();
    }
    stored.rsplit('/').next().unwrap_or(stored).to_string()
}

impl DocAttachmentRow {
    /// 从查询结果行构造。
    ///
    /// # 异常情况
    ///
    /// 列类型不匹配时返回 [`DbError`]（说明 SQL 与行结构漂移了）。
    fn from_row(row: &Row) -> DbResult<DocAttachmentRow> {
        let original: Option<String> = row.get_typed("original_name")?;
        let stored: String = row.get_typed("stored_name")?;
        Ok(DocAttachmentRow {
            id: row.get_typed("id")?,
            doc_type: row.get_typed("doc_type")?,
            doc_id: row.get_typed("doc_id")?,
            file_id: row.get_typed("file_id")?,
            file_role: row.get_typed("file_role")?,
            remark: row.get_typed("remark")?,
            uploaded_by: row.get_typed("uploaded_by")?,
            uploader_name: row.get_typed("uploader_name")?,
            file_name: display_name(original.as_deref(), &stored),
            file_size: row.get_typed("file_size")?,
            mime_type: row.get_typed("mime_type")?,
            sort_order: row.get_typed("sort_order")?,
            created_at: row.get_typed("created_at")?,
        })
    }
}

/// 单据是否存在（含软删判定）。
///
/// # 参数
///
/// - `kind`：单据类型（决定查哪张表）；
/// - `doc_id`：单据主键。
///
/// # 返回值
///
/// 存在且未软删返回 `true`。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn doc_exists(kind: DocKind, doc_id: &Uuid) -> DbResult<bool> {
    let rows = db()
        .find_with_params(kind.exists_sql(), vec![Value::from(doc_id)])
        .await?;
    Ok(!rows.is_empty())
}

/// 当前单据的未删附件数（配额判定；与 [`list_by_doc`] 同口径）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn count_by_doc(kind: DocKind, doc_id: &Uuid) -> DbResult<i64> {
    db().scalar_with_params(
        "SELECT count(*) FROM biz_doc_attachment \
          WHERE doc_type = ? AND doc_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::Text(kind.as_str().to_string()), Value::from(doc_id)],
    )
    .await
}

/// 同一份文件还被多少个**未删**绑定引用（软删 `sys_file` 之前的判据）。
///
/// 秒传（内容 hash 命中）会让多张单据共用一条 `sys_file`：删一张单据的附件时
/// 不能顺手把文件记录软删，否则别的单据的附件会变成「记录在、文件已删」。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn count_by_file(file_id: &Uuid) -> DbResult<i64> {
    db().scalar_with_params(
        "SELECT count(*) FROM biz_doc_attachment \
          WHERE file_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(file_id)],
    )
    .await
}

/// 绑定一行，并**落库上传人姓名快照**（`biz_doc_attachment.uploader_name`，AUD-01）。
///
/// # 参数
///
/// - `kind` / `doc_id`：单据；
/// - `file_id`：`sys_file.id`（调用方已确认该记录有效）；
/// - `role`：文件角色；
/// - `remark`：备注；
/// - `uploaded_by`：上传人 id（行级删除判据用它）；
/// - `uploader_name`：上传人姓名快照（[`crate::business::shared::user_name::snapshot_name`]
///   的返回值）。**写进库后永不回改** —— 用户改名 / 销号后，历史附件仍显示当时的名字。
///
/// # 为什么姓名要单独传，不在 SQL 里 `JOIN sys_user` 现取
///
/// 现取只能拿到「此刻的名字」，而本列的全部价值就在「当时是谁」。handler 侧显式
/// `let name = snapshot_name(uid).await?;` 有三个好处：① 写库语句只有一处依赖 sys_user
/// （读的时候不需要再 join 才能解释这一列）；② 追溯不到人（已删 / 行不存在）时的
/// 占位串由 [`crate::business::shared::user_name`] 统一给出，不散落在 SQL 字面量里；
/// ③ 测试夹具可以不传（`None`），不会因为「库里没这个用户」而写失败。
///
/// # 返回值
///
/// 绑定行主键。
///
/// # 排序序号的写法（ATT-03）
///
/// `sort_order = COALESCE(max(sort_order) + 1, 0)` **在同一条 INSERT 里算**：
///
/// - 不能让新行吃列的 `DEFAULT 0` —— 用户手排过「A,B,C」后新传 D，D 会插到最前面；
/// - 也不在 handler 里「先 SELECT max 再 INSERT」—— 两次往返之间有竞态窗口，
///   两个人同时上传会拿到同一个序号（同序号虽然能靠 created_at 兜底，但排序就不稳定了）。
///
/// 子查询在表上没有行时返回 NULL → `COALESCE(..., 0)` 让第一行从 0 开始。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]（外键失败说明 `file_id` 不存在，调用方应先校验）。
pub async fn insert_with_uploader(
    kind: DocKind,
    doc_id: &Uuid,
    file_id: &Uuid,
    role: FileRole,
    remark: Option<&str>,
    uploaded_by: Option<Uuid>,
    uploader_name: Option<&str>,
) -> DbResult<Uuid> {
    let rows = db()
        .find_with_params(
            "INSERT INTO biz_doc_attachment \
                 (id, doc_type, doc_id, file_id, file_role, remark, uploaded_by, uploader_name, \
                  created_at, sort_order) \
             VALUES (?::uuid, ?, ?::uuid, ?::uuid, ?, ?, ?::uuid, ?, now(), \
                     COALESCE((SELECT max(x.sort_order) + 1 FROM biz_doc_attachment x \
                                WHERE x.doc_type = ? AND x.doc_id = ?::uuid AND x.deleted_at IS NULL), 0)) \
             ON CONFLICT (doc_type, doc_id, file_id) WHERE deleted_at IS NULL DO NOTHING \
             RETURNING id",
            vec![
                Value::from(&Uuid::now_v7()),
                Value::Text(kind.as_str().to_string()),
                Value::from(doc_id),
                Value::from(file_id),
                Value::Text(role.as_str().to_string()),
                remark.map_or(Value::Null, |text| Value::Text(text.to_string())),
                uploaded_by.map_or(Value::Null, |id| Value::from(&id)),
                uploader_name.map_or(Value::Null, |text| Value::Text(text.to_string())),
                // 子查询的两个占位：与上面同一口径的 (doc_type, doc_id)
                Value::Text(kind.as_str().to_string()),
                Value::from(doc_id),
            ],
        )
        .await?;
    if let Some(row) = rows.first() {
        return row.get_typed("id");
    }

    // 已绑定过（并发或重复上传同一份字节）：返回既有行主键，不报错。
    // **不回写姓名**：既有行的上传人是谁，就保留谁的名字（重复上传同一份字节不算换人）。
    let existing = db()
        .find_with_params(
            "SELECT id FROM biz_doc_attachment \
              WHERE doc_type = ? AND doc_id = ?::uuid AND file_id = ?::uuid AND deleted_at IS NULL",
            vec![
                Value::Text(kind.as_str().to_string()),
                Value::from(doc_id),
                Value::from(file_id),
            ],
        )
        .await?;
    existing
        .first()
        .ok_or_else(|| DbError::InvalidValue("附件绑定冲突且查不到既有行".to_string()))?
        .get_typed("id")
}

/// 绑定一行（**不带姓名快照**）—— 只给测试夹具用。
///
/// # 为什么它只在 `#[cfg(test)]` 里
///
/// 生产路径必须写姓名快照（AUD-01 的验收②），给一个「可以不传姓名」的生产入口，
/// 早晚会有人在某个新分支里调它，然后界面上又出现 UUID。
/// 于是把它收成测试专用：夹具库里不一定有 `sys_user` 行，读路径会按
/// `COALESCE(NULLIF(a.uploader_name,''), <实时 join>)` 回落到现值，断言不受影响。
///
/// 实现在 [`insert_with_uploader`]（`uploader_name = None`）。
#[cfg(test)]
pub async fn insert(
    kind: DocKind,
    doc_id: &Uuid,
    file_id: &Uuid,
    role: FileRole,
    remark: Option<&str>,
    uploaded_by: Option<Uuid>,
) -> DbResult<Uuid> {
    insert_with_uploader(kind, doc_id, file_id, role, remark, uploaded_by, None).await
}

/// 按单据列附件（未删）。
///
/// # 排序（ATT-03）
///
/// 由 [`ListOrder`] 决定，SQL 常量只从 [`ListOrder::order_by`] 取：
///
/// - [`ListOrder::TimeDesc`]（默认）：`created_at DESC, id DESC` —— 刚传的在上；
/// - [`ListOrder::Manual`]：`sort_order ASC, created_at DESC, id DESC` —— 用户手排的顺序。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn list_by_doc(
    kind: DocKind,
    doc_id: &Uuid,
    order: ListOrder,
) -> DbResult<Vec<DocAttachmentRow>> {
    let sql = format!(
        "SELECT {COLUMNS} \
           FROM biz_doc_attachment a \
           LEFT JOIN sys_file f ON f.id = a.file_id \
           LEFT JOIN sys_user u ON u.id = a.uploaded_by \
          WHERE a.doc_type = ? AND a.doc_id = ?::uuid AND a.deleted_at IS NULL \
          {}",
        order.order_by()
    );
    let rows = db()
        .find_with_params(
            &sql,
            vec![Value::Text(kind.as_str().to_string()), Value::from(doc_id)],
        )
        .await?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        list.push(DocAttachmentRow::from_row(row)?);
    }
    Ok(list)
}

/// 当前单据**未删附件**的主键（按 `sort_order` 升序；重排前的「成员校验」用）。
///
/// # 为什么要单独一条（不直接用 [`list_by_doc`]）
///
/// 重排只需要 id 集合：走 `list_by_doc` 会连带读 `sys_file` / `sys_user`（两表 JOIN），
/// 在「同一张单据 50 个附件」时是白读。这条 SQL 只碰一张表。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn active_ids(kind: DocKind, doc_id: &Uuid) -> DbResult<Vec<Uuid>> {
    let rows = db()
        .find_with_params(
            "SELECT id FROM biz_doc_attachment \
              WHERE doc_type = ? AND doc_id = ?::uuid AND deleted_at IS NULL \
              ORDER BY sort_order ASC, created_at ASC, id ASC",
            vec![Value::Text(kind.as_str().to_string()), Value::from(doc_id)],
        )
        .await?;
    let mut ids = Vec::with_capacity(rows.len());
    for row in &rows {
        ids.push(row.get_typed("id")?);
    }
    Ok(ids)
}

/// 按给定顺序重写 `sort_order`（`ids[0]` → 0、`ids[1]` → 1 …）。
///
/// # 为什么逐条 UPDATE 而不是一条 `unnest(uuid[], int[])`
///
/// 数组参数在本仓是已知的编码陷阱（`Vec<Uuid>` 驱动编不上，得绕
/// `Value::Text("{a,b}")` + `?::text::uuid[]`）。单据附件上限
/// [`MAX_ATTACHMENTS_PER_DOC`] = 50，逐条 UPDATE 最多 50 次主键命中，
/// 换来的是**不依赖任何数组编码细节**的可读实现。调用方把它放进
/// `db().transaction(..)` 里，50 条语句仍是一次提交。
///
/// # 参数
///
/// - `ex`：执行器（`db()` 或事务 `tx`）；
/// - `kind` / `doc_id`：单据（**写库条件的一部分**：越权传别的单据的附件 id 只会命中 0 行）；
/// - `ids`：目标顺序。
///
/// # 返回值
///
/// 实际更新的行数（`< ids.len()` 说明有 id 不属于该单据 / 已被删）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn set_sort_orders(
    ex: &impl SqlExecutor,
    kind: DocKind,
    doc_id: &Uuid,
    ids: &[Uuid],
) -> DbResult<u64> {
    let mut updated = 0u64;
    for (index, id) in ids.iter().enumerate() {
        // 序号从 0 起：`sort_order` 是 `integer`，50 条附件远不到 i32 上限
        let position = i32::try_from(index).unwrap_or(i32::MAX);
        updated += ex
            .exec_sql(
                "UPDATE biz_doc_attachment SET sort_order = ?::int \
                  WHERE id = ?::uuid AND doc_type = ? AND doc_id = ?::uuid AND deleted_at IS NULL"
                    .to_string(),
                vec![
                    Value::I32(position),
                    Value::from(id),
                    Value::Text(kind.as_str().to_string()),
                    Value::from(doc_id),
                ],
            )
            .await?;
    }
    Ok(updated)
}

/// 取单条未删附件（下载 / 删除前的归属校验）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn find_active(id: &Uuid) -> DbResult<Option<DocAttachmentRow>> {
    let sql = format!(
        "SELECT {COLUMNS} \
           FROM biz_doc_attachment a \
           LEFT JOIN sys_file f ON f.id = a.file_id \
           LEFT JOIN sys_user u ON u.id = a.uploaded_by \
          WHERE a.id = ?::uuid AND a.deleted_at IS NULL"
    );
    let rows = db().find_with_params(&sql, vec![Value::from(id)]).await?;
    match rows.first() {
        Some(row) => Ok(Some(DocAttachmentRow::from_row(row)?)),
        None => Ok(None),
    }
}

/// 软删绑定行（只写 `deleted_at`：绑定关系是一次性事实，没有 `updated_at` / `update_by`）。
///
/// # 返回值
///
/// 影响行数（0 = 已经被别人删掉了，调用方按幂等处理）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn soft_delete(ex: &impl SqlExecutor, id: &Uuid) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_doc_attachment SET deleted_at = now() \
          WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![Value::from(id)],
    )
    .await
}

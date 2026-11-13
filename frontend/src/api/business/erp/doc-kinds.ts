/**
 * 业务类型词表（**唯一真相**，前端侧）—— P2-5。
 *
 * # 与后端的关系
 *
 * 后端的唯一真相是 `backend/src/business/shared/doc_kind.rs`（七根轴、同一套大写下划线拼写）。
 * 本文件是它的**逐字镜像**：数组顺序、取值、标签一一对应。
 *
 * 两边一致性由后端单测钉死 —— `backend/src/business/shared/doc_kind_tests.rs`
 * 里的 `frontend_ts_mirror_matches_rust` 会直接读本文件、解析下面那几个 `as const` 数组，
 * 与 Rust 枚举逐字比对。**改了这里不改 Rust（或反过来）会直接单测失败**，
 * 这就是「不留第二套词表」的机械保证。
 *
 * # 为什么前端不自己去后端取
 *
 * 取过就变成运行期依赖：接口一慢页面就没标签、离线/首屏渲染要等。
 * 这些是**枚举**（不是配置），跟着代码走才对；后端新增类型时本来就要发版。
 *
 * # 七根轴
 *
 * | 轴 | 常量 | 落库列 |
 * |---|---|---|
 * | ① 业务对象 | {@link DOC_KINDS} | `biz_status_log.entity_type`、`biz_doc_attachment.doc_type`、`biz_reminder_log.doc_type` |
 * | ② 审批动作 | {@link FLOW_BIZ_TYPES} | `biz_flow_def.biz_type`、`biz_flow_instance.biz_type` |
 * | ③ 库存变动动作 | {@link TXN_BIZ_TYPES} | `biz_inventory_txn.biz_type`、`biz_voucher_rule.biz_type` |
 * | ④ 流水来源单据 | {@link TXN_DOC_TYPES} | `biz_inventory_txn.doc_type` |
 * | ⑤ 凭证来源 | {@link VOUCHER_SOURCE_TYPES} | `biz_voucher.source_type`、`biz_payable.source_type` |
 * | ⑥ 序列号来源 | {@link SERIAL_SOURCE_TYPES} | `biz_material_serial.source_type` |
 * | ⑦ 单据子类型 | {@link IN_TYPES} / {@link OUT_TYPES} / {@link TAKE_TYPES} | `biz_stock_in.in_type`、`biz_stock_out.out_type`、`biz_stock_take.take_type` |
 *
 * # 禁止
 *
 * 任何 `.ts` / `.vue` 里**不许再写**这些取值字面量（`'PO_APPROVE'` / `'STOCK_IN'` / …），
 * 一律从这里取常量。需要新值 → 先加后端枚举，再镜像到这里，否则单测红。
 */

// ==================== ① 业务对象 ====================

// ATT-11：末尾加了主数据（SUPPLIER / CUSTOMER）与期初余额（OPENING_BALANCE）——
// 它们不是可审批单据，但都是有实体表的**业务对象**（附件 / 提醒要挂在上面）。
/** 业务对象类型（大写下划线，落库值）。顺序与 Rust `DocKind::ALL` 逐字一致。 */
export const DOC_KINDS = [
  'PURCHASE_ORDER',
  'STOCK_IN',
  'STOCK_OUT',
  'STOCK_TRANSFER',
  'STOCK_TAKE',
  'PAYABLE',
  'PAYMENT',
  'VOUCHER',
  'CONTRACT',
  'INVENTORY',
  'ACCOUNTING_PERIOD',
  'ACCOUNTING_YEAR',
  'FLOW_EDGE_CFG',
  'FLOW_LINKAGE_CFG',
  'BUDGET',
  'FIXED_ASSET',
  'DEPRECIATION',
  'EXPENSE_REPORT',
  'SALE_ORDER',
  'SHIPMENT',
  'RECEIVABLE',
  'RECEIPT',
  'RECEIPT_REFUND',
  'INVOICE',
  'SALES_RETURN',
  'PURCHASE_RETURN',
  'SUPPLIER',
  'CUSTOMER',
  'OPENING_BALANCE',
  'COST_OBJECT',
  'COST_PERIOD',
  'EMPLOYEE',
  'EMPLOYEE_CONTRACT',
  'LEAVE_REQUEST',
  'ATTENDANCE_PERIOD',
  'SALARY_SHEET',
] as const;

/** 业务对象类型（联合类型 —— 写错编译期就报错）。 */
export type DocKind = (typeof DOC_KINDS)[number];

/** 业务对象 → 中文（与 Rust `DocKind::label` 逐字一致）。 */
export const DOC_KIND_LABELS: Readonly<Record<DocKind, string>> = {
  PURCHASE_ORDER: '采购订单',
  STOCK_IN: '收货入库单',
  STOCK_OUT: '出库单',
  STOCK_TRANSFER: '库存调拨单',
  STOCK_TAKE: '盘点单',
  PAYABLE: '应付单',
  PAYMENT: '付款单',
  VOUCHER: '凭证',
  CONTRACT: '合同',
  INVENTORY: '库存',
  ACCOUNTING_PERIOD: '会计期间',
  ACCOUNTING_YEAR: '会计年度',
  FLOW_EDGE_CFG: '状态机配置',
  FLOW_LINKAGE_CFG: '联动规则配置',
  BUDGET: '预算',
  FIXED_ASSET: '固定资产',
  DEPRECIATION: '折旧批次',
  EXPENSE_REPORT: '费用报销单',
  SALE_ORDER: '销售订单',
  SHIPMENT: '发货单',
  RECEIVABLE: '应收单',
  RECEIPT: '收款单',
  RECEIPT_REFUND: '收款退款单',
  INVOICE: '销售发票',
  SALES_RETURN: '销售退货单',
  PURCHASE_RETURN: '采购退货单',
  // ATT-11
  SUPPLIER: '供应商',
  CUSTOMER: '客户',
  OPENING_BALANCE: '期初余额',
  // 成本核算（ARCH-9）
  COST_OBJECT: '成本对象',
  COST_PERIOD: '成本核算批次',
  EMPLOYEE: '员工',
  EMPLOYEE_CONTRACT: '劳动合同',
  LEAVE_REQUEST: '请假单',
  ATTENDANCE_PERIOD: '考勤期间',
  SALARY_SHEET: '薪资单',
};

/**
 * 业务对象的便捷常量（模板里用 `:doc-type="DOC_KIND.PURCHASE_ORDER"`，
 * 免得模板里散落字面量）。
 *
 * 类型是映射类型 `{ [K in DocKind]: K }`：每个键的类型就是**它自己的字面量**，
 * 所以 `DOC_KIND.PURCHASE_ORDER` 的类型是 `'PURCHASE_ORDER'`（能赋给收窄过的
 * `DocType`），而键集合由 `DOC_KINDS` 决定 —— 既不重复写字面量，也漏不掉新值。
 */
export const DOC_KIND = Object.freeze(
  Object.fromEntries(DOC_KINDS.map((kind) => [kind, kind])),
) as { readonly [K in DocKind]: K };

/** 业务对象 → 实体表（虚拟对象无表，未列出即为 `undefined`）。 */
export const DOC_KIND_TABLES: Readonly<Partial<Record<DocKind, string>>> = {
  PURCHASE_ORDER: 'biz_purchase_order',
  STOCK_IN: 'biz_stock_in',
  STOCK_OUT: 'biz_stock_out',
  STOCK_TRANSFER: 'biz_stock_transfer',
  STOCK_TAKE: 'biz_stock_take',
  PAYABLE: 'biz_payable',
  PAYMENT: 'biz_payment',
  VOUCHER: 'biz_voucher',
  CONTRACT: 'biz_contract',
  INVENTORY: 'biz_inventory',
  ACCOUNTING_PERIOD: 'biz_accounting_period',
  FLOW_EDGE_CFG: 'biz_flow_edge',
  FLOW_LINKAGE_CFG: 'biz_flow_linkage_rule',
  BUDGET: 'biz_budget',
  FIXED_ASSET: 'biz_fixed_asset',
  DEPRECIATION: 'biz_depreciation_batch',
  EXPENSE_REPORT: 'biz_expense_report',
  SALE_ORDER: 'biz_sales_order',
  SHIPMENT: 'biz_shipment',
  RECEIVABLE: 'biz_receivable',
  RECEIPT: 'biz_receipt',
  RECEIPT_REFUND: 'biz_receipt_refund',
  INVOICE: 'biz_invoice',
  SALES_RETURN: 'biz_sales_return',
  PURCHASE_RETURN: 'biz_purchase_return',
  // ATT-11：期初余额没有单头表，锚点是期初凭证（doc_id = 期初凭证 id）
  SUPPLIER: 'biz_supplier',
  CUSTOMER: 'biz_customer',
  OPENING_BALANCE: 'biz_voucher',
  // 成本核算（ARCH-9）：生产批主表
  COST_OBJECT: 'biz_cost_object',
  COST_PERIOD: 'biz_cost_period',
  EMPLOYEE: 'biz_employee',
  EMPLOYEE_CONTRACT: 'biz_employee_contract',
  LEAVE_REQUEST: 'biz_leave_request',
  ATTENDANCE_PERIOD: 'biz_attendance_period',
  SALARY_SHEET: 'biz_salary_sheet',
};

/**
 * 业务对象 → REST 路径段（**唯一**允许的小写连字符写法）。
 *
 * 联动作废的 URL 是 `/{docType}/{id}/cancel-impact`，`{docType}` 只能取本表的 value。
 */
export const DOC_KIND_ROUTES = {
  PURCHASE_ORDER: 'purchase-order',
  STOCK_IN: 'stock-in',
  PAYABLE: 'payable',
  VOUCHER: 'voucher',
  STOCK_OUT: 'stock-out',
  STOCK_TRANSFER: 'stock-transfer',
  RECEIPT: 'receipt',
} as const;

/** 支持联动作废的业务对象（`/cancel-impact`、`/cancel-cascade` 的可选项）。 */
export type CancelDocType = keyof typeof DOC_KIND_ROUTES;

// ATT-11：ATTACHMENT_DOC_TYPES 末尾加 4 类「建议传（不校验）」的对象：
// 调拨单 / 供应商 / 客户 / 期初余额（V0350 把 DDL 的 CHECK 放宽到 16 值）。
/**
 * 能挂业务附件的业务对象（`biz_doc_attachment.doc_type` 的 17 类）。
 *
 * 与后端 `doc_kind::ATTACHMENT_DOC_KINDS` + DDL 的 `ck_biz_doc_attachment_type`
 * 逐字一致（后端单测 `attachment_doc_types_match_rust` 会比对顺序与取值）。
 * **词表里不要写注释**：那个单测是逐个 `'大写值'` 抓字面量的，中间夹一行中文注释
 * 会被当成一个「取值」而判红（NEAR-FIX-1 实测踩过）。
 *
 * ATT-08 从 7 类补到 12 类：ATT-06 把 DDL 放宽到 12 值（发票 / 收款 / 盘点 /
 * 采购退货 / 销售退货 也能挂附件），但前端词表当时没跟上 —— 结果是「发票必须传
 * 发票影像」这条规则在界面上无法满足（上传下拉里没有发票）。补齐之后
 * 单据详情页才能真的传上那份影像，闸门才有意义。
 */
export const ATTACHMENT_DOC_TYPES = [
  'CONTRACT',
  'PURCHASE_ORDER',
  'STOCK_IN',
  'PAYABLE',
  'PAYMENT',
  'FIXED_ASSET',
  'EXPENSE_REPORT',
  'INVOICE',
  'RECEIPT',
  'STOCK_TAKE',
  'PURCHASE_RETURN',
  'SALES_RETURN',
  'STOCK_TRANSFER',
  'SUPPLIER',
  'CUSTOMER',
  'OPENING_BALANCE',
  'RECEIPT_REFUND',
  'EMPLOYEE',
  'EMPLOYEE_CONTRACT',
  'LEAVE_REQUEST',
  'SALARY_SHEET',
] as const;

/** 能挂业务附件的业务对象（联合类型）。 */
export type AttachmentDocType = (typeof ATTACHMENT_DOC_TYPES)[number];

// ==================== ② 审批动作 ====================

// 销售 / 采购 / 成本：**存量补齐**（t_c49010bf）。这 4 个值早就在 `biz_flow_def` 里
// （V0281 / V0331 / V0332 / V0381 种的标准流程），只是后端枚举漏登记。位置 = Rust
// `FlowBizType::ALL` 的末尾 4 项（追加在末尾、不插中间：两边是保序逐字比对）。
/** 审批业务类型（`biz_flow_def.biz_type`）。顺序与 Rust `FlowBizType::ALL` 逐字一致。 */
export const FLOW_BIZ_TYPES = [
  'PO_APPROVE',
  'STOCK_IN_POST',
  'STOCK_IN_CANCEL',
  'STOCK_TAKE_POST',
  'STOCK_TRANSFER',
  'PAYABLE_PAY',
  'PAYABLE_CANCEL',
  'VOUCHER_REVERSE',
  'CONTRACT_APPROVE',
  'TASK_ACCEPT',
  'FLOW_EDGE_CFG',
  'EXPENSE',
  'OTHER',
  'BUDGET_APPROVE',
  'ASSET_DISPOSAL',
  'RECEIPT_REFUND',
  'HR_LEAVE',
  'HR_SALARY',
  'HR_SALARY_ADJUST',
  'HR_EMPLOYEE_RESIGN',
  'SO_APPROVE',
  'PR_APPROVE',
  'PRT_APPROVE',
  'COST_SETTLE',
] as const;

/** 审批业务类型（联合类型）。 */
export type FlowBizType = (typeof FLOW_BIZ_TYPES)[number];

// 末尾 4 项（销售 / 采购 / 成本，存量补齐）与上面 `FLOW_BIZ_TYPES` 的末尾 4 项同序。
/** 审批业务类型 → 中文（与 Rust `FlowBizType::label` 逐字一致）。 */
export const FLOW_BIZ_TYPE_LABELS: Readonly<Record<FlowBizType, string>> = {
  PO_APPROVE: '采购订单审批',
  STOCK_IN_POST: '收货过账审批',
  STOCK_IN_CANCEL: '收货作废审批',
  STOCK_TAKE_POST: '盘点过账审批',
  STOCK_TRANSFER: '库存调拨审批',
  PAYABLE_PAY: '应付付款审批',
  PAYABLE_CANCEL: '应付作废审批',
  VOUCHER_REVERSE: '凭证红冲审批',
  CONTRACT_APPROVE: '合同审批',
  TASK_ACCEPT: '任务验收',
  FLOW_EDGE_CFG: '状态机配置变更审批',
  EXPENSE: '费用审批',
  OTHER: '其他审批',
  BUDGET_APPROVE: '预算审批',
  ASSET_DISPOSAL: '资产处置审批',
  RECEIPT_REFUND: '收款退款审批',
  HR_LEAVE: '请假审批',
  HR_SALARY: '工资发放审批',
  HR_SALARY_ADJUST: '调薪审批',
  HR_EMPLOYEE_RESIGN: '离职审批',
  SO_APPROVE: '销售订单审批',
  PR_APPROVE: '采购申请审批',
  PRT_APPROVE: '采购退货审批',
  COST_SETTLE: '成本结算审批',
};

// 末尾 4 项的 flow_code 与各自迁移种下的值逐字一致（成本那条不是 `cost_settle`，是 `cost_settle_approve`）。
/** 审批业务类型 → 默认流程编码（`biz_flow_def.flow_code`）。 */
export const FLOW_BIZ_TYPE_FLOW_CODES: Readonly<Record<FlowBizType, string>> = {
  PO_APPROVE: 'po_approve',
  STOCK_IN_POST: 'stock_in_post',
  STOCK_IN_CANCEL: 'stock_in_cancel',
  STOCK_TAKE_POST: 'stock_take_post',
  STOCK_TRANSFER: 'stock_transfer',
  PAYABLE_PAY: 'payable_pay',
  PAYABLE_CANCEL: 'payable_cancel',
  VOUCHER_REVERSE: 'voucher_reverse',
  CONTRACT_APPROVE: 'contract_approve',
  TASK_ACCEPT: 'task_accept',
  FLOW_EDGE_CFG: 'flow_edge_config',
  EXPENSE: 'expense',
  OTHER: 'other',
  BUDGET_APPROVE: 'budget_approve',
  ASSET_DISPOSAL: 'asset_disposal',
  RECEIPT_REFUND: 'receipt_refund',
  HR_LEAVE: 'hr_leave',
  HR_SALARY: 'hr_salary',
  HR_SALARY_ADJUST: 'hr_salary_adjust',
  HR_EMPLOYEE_RESIGN: 'hr_employee_resign',
  SO_APPROVE: 'so_approve',
  PR_APPROVE: 'pr_approve',
  PRT_APPROVE: 'prt_approve',
  COST_SETTLE: 'cost_settle_approve',
};

/** 采购订单审批的业务类型（历史常量名，值来自 {@link FLOW_BIZ_TYPES}）。 */
export const PO_APPROVE_BIZ_TYPE: FlowBizType = 'PO_APPROVE';

// ==================== ③ 库存变动动作 ====================

/** 库存流水业务动作（`biz_inventory_txn.biz_type`）。顺序与 Rust `TxnBizType::ALL` 逐字一致。 */
export const TXN_BIZ_TYPES = [
  'PURCHASE_IN',
  'PURCHASE_IN_CANCEL',
  'PURCHASE_RETURN',
  'SALE_OUT',
  'PRODUCTION_IN',
  'ADJUST_IN',
  'ADJUST_OUT',
  'TRANSFER_OUT',
  'TRANSFER_IN',
  'ISSUE_OUT',
  'SCRAP_OUT',
  'LOSS_OUT',
  'OTHER_OUT',
  'STOCK_OUT_CANCEL',
  'PRODUCE_CANCEL',
  'SERIAL_IN',
  'SERIAL_OUT',
  'SERIAL_SCRAP',
  'OPENING',
  'STOCK_TAKE_GAIN',
  'STOCK_TAKE_LOSS',
  'STOCK_TAKE_CANCEL',
  'SALES_RETURN_IN',
] as const;

/** 库存流水业务动作（联合类型）。 */
export type TxnBizType = (typeof TXN_BIZ_TYPES)[number];

/** 库存流水业务动作 → 中文（与 Rust `TxnBizType::label` 逐字一致）。 */
export const TXN_BIZ_TYPE_LABELS: Readonly<Record<TxnBizType, string>> = {
  PURCHASE_IN: '采购入库',
  PURCHASE_IN_CANCEL: '采购入库红冲',
  PURCHASE_RETURN: '采购退货',
  SALE_OUT: '销售出库',
  PRODUCTION_IN: '生产入库',
  ADJUST_IN: '盘盈入库',
  ADJUST_OUT: '盘亏出库',
  TRANSFER_OUT: '调拨出库',
  TRANSFER_IN: '调拨入库',
  ISSUE_OUT: '领用出库',
  SCRAP_OUT: '报废出库',
  LOSS_OUT: '盘亏出库',
  OTHER_OUT: '其他出库',
  STOCK_OUT_CANCEL: '出库作废',
  PRODUCE_CANCEL: '生产入库红冲',
  SERIAL_IN: '序列号入库',
  SERIAL_OUT: '序列号出库',
  SERIAL_SCRAP: '序列号报废',
  OPENING: '期初导入',
  STOCK_TAKE_GAIN: '盘点盘盈',
  STOCK_TAKE_LOSS: '盘点盘亏',
  STOCK_TAKE_CANCEL: '盘点作废回补',
  SALES_RETURN_IN: '销售退货入库',
};

// ==================== ④ 流水来源单据 ====================

/** 库存流水来源单据（`biz_inventory_txn.doc_type`）。顺序与 Rust `TxnDocType::ALL` 逐字一致。 */
export const TXN_DOC_TYPES = [
  'STOCK_IN',
  'STOCK_OUT',
  'STOCK_TRANSFER',
  'STOCK_TAKE',
  'SERIAL_BATCH',
  'MATERIAL_SERIAL',
  'OPENING',
  'ADJUST',
] as const;

/** 库存流水来源单据（联合类型）。 */
export type TxnDocType = (typeof TXN_DOC_TYPES)[number];

/** 库存流水来源单据 → 中文（与 Rust `TxnDocType::label` 逐字一致）。 */
export const TXN_DOC_TYPE_LABELS: Readonly<Record<TxnDocType, string>> = {
  STOCK_IN: '收货入库单',
  STOCK_OUT: '出库单',
  STOCK_TRANSFER: '库存调拨单',
  STOCK_TAKE: '盘点单',
  SERIAL_BATCH: '序列号批次',
  MATERIAL_SERIAL: '序列号',
  OPENING: '期初导入',
  ADJUST: '库存调整',
};

// ==================== ⑤ 凭证来源 ====================

/** 凭证来源（`biz_voucher.source_type`）。顺序与 Rust `VoucherSourceType::ALL` 逐字一致。 */
export const VOUCHER_SOURCE_TYPES = [
  'STOCK_IN',
  'STOCK_OUT',
  'PAYMENT',
  'STOCK_TAKE',
  'SERIAL_BATCH',
  'SERIAL_OUT',
  'SERIAL_SCRAP',
  'OPENING',
  'YEAR_END',
  'MANUAL',
  'FIXED_ASSET_ACQ',
  'FIXED_ASSET_DISPOSAL',
  'DEPRECIATION',
  'EXPENSE_REPORT',
  'EXPENSE_PAYMENT',
  'RECEIPT',
  'RECEIPT_REFUND',
  'INVOICE',
  'COMMISSION',
  'COST_PERIOD',
  'PAYROLL_ACCRUAL',
  'PAYROLL_PAYMENT',
] as const;

/** 凭证来源（联合类型）。 */
export type VoucherSourceType = (typeof VOUCHER_SOURCE_TYPES)[number];

/** 凭证来源 → 中文（与 Rust `VoucherSourceType::label` 逐字一致）。 */
export const VOUCHER_SOURCE_TYPE_LABELS: Readonly<Record<VoucherSourceType, string>> = {
  STOCK_IN: '收货过账',
  STOCK_OUT: '出库过账',
  PAYMENT: '应付付款',
  STOCK_TAKE: '盘点过账',
  SERIAL_BATCH: '序列号批次',
  SERIAL_OUT: '序列号出库',
  SERIAL_SCRAP: '序列号报废',
  OPENING: '期初录入',
  YEAR_END: '年结结转',
  MANUAL: '手工录入',
  FIXED_ASSET_ACQ: '固定资产购入',
  FIXED_ASSET_DISPOSAL: '固定资产处置',
  DEPRECIATION: '折旧计提',
  EXPENSE_REPORT: '费用报销入账',
  EXPENSE_PAYMENT: '费用报销付款',
  RECEIPT: '收款过账',
  RECEIPT_REFUND: '收款退款过账',
  INVOICE: '发票开票',
  COMMISSION: '佣金结算',
  COST_PERIOD: '成本核算结算',
  PAYROLL_ACCRUAL: '工资计提',
  PAYROLL_PAYMENT: '工资发放',
};

// ==================== ⑥ 序列号来源 ====================

/** 序列号来源（`biz_material_serial.source_type`）。顺序与 Rust `SerialSourceType::ALL` 逐字一致。 */
export const SERIAL_SOURCE_TYPES = ['PURCHASE', 'TRANSFER', 'PRODUCE', 'RETURN'] as const;

/** 序列号来源（联合类型）。 */
export type SerialSourceType = (typeof SERIAL_SOURCE_TYPES)[number];

/** 序列号来源 → 中文（与 Rust `SerialSourceType::label` 逐字一致）。 */
export const SERIAL_SOURCE_TYPE_LABELS: Readonly<Record<SerialSourceType, string>> = {
  PURCHASE: '采购入库',
  TRANSFER: '调拨入库',
  PRODUCE: '生产入库',
  RETURN: '退货入库',
};

// ==================== ⑦ 单据子类型 ====================

/**
 * 收货单子类型（`biz_stock_in.in_type`，与 `ck_biz_si_type` 一致）。
 *
 * `SOURCELESS` = 无源入库（`is_sourceless=true`：没有采购单、但有供应商），
 * 与 `OTHER`（其他入库：没有采购单也没有供应商）分开，报表按 in_type 就能数出「没走采购的入库」。
 */
export const IN_TYPES = ['PURCHASE', 'RETURN', 'OTHER', 'SOURCELESS'] as const;
/** 收货单子类型（联合类型）。 */
export type InType = (typeof IN_TYPES)[number];

/** 出库单子类型（`biz_stock_out.out_type`，与 `ck_biz_so_type` 一致）。 */
export const OUT_TYPES = ['ISSUE', 'SCRAP', 'LOSS', 'OTHER'] as const;
/** 出库单子类型（联合类型）。 */
export type OutType = (typeof OUT_TYPES)[number];

/** 盘点方式（`biz_stock_take.take_type`，与 `ck_biz_st_type` 一致）。 */
export const TAKE_TYPES = ['FULL', 'PARTIAL'] as const;
/** 盘点方式（联合类型）。 */
export type TakeType = (typeof TAKE_TYPES)[number];

/** 收货子类型 → 中文（与 Rust `DocSubtype::label` 一致）。 */
export const IN_TYPE_LABELS: Readonly<Record<InType, string>> = {
  PURCHASE: '采购入库',
  RETURN: '退货入库',
  OTHER: '其他入库',
  SOURCELESS: '无源入库',
};

/** 出库子类型 → 中文（与 Rust `DocSubtype::label` 一致）。 */
export const OUT_TYPE_LABELS: Readonly<Record<OutType, string>> = {
  ISSUE: '领用出库',
  SCRAP: '报废出库',
  LOSS: '盘亏出库',
  OTHER: '其他出库',
};

// ==================== 通用小工具 ====================

/**
 * 取中文标签；**未知值原样返回**（不猜 —— 后端加了新码，页面上显示英文码
 * 也比显示一个错的中文强，而且一眼能看出是「前端词表没跟上」）。
 */
export function labelOf(
  labels: Readonly<Record<string, string>>,
  raw: string | null | undefined,
): string {
  if (!raw) {
    return '—';
  }
  return labels[raw] ?? String(raw);
}

/** 业务对象中文。 */
export function docKindLabel(raw: string | null | undefined): string {
  return labelOf(DOC_KIND_LABELS, raw);
}

/** 审批业务类型中文。 */
export function flowBizTypeLabel(raw: string | null | undefined): string {
  return labelOf(FLOW_BIZ_TYPE_LABELS, raw);
}

/** 库存流水业务动作中文。 */
export function txnBizTypeLabel(raw: string | null | undefined): string {
  return labelOf(TXN_BIZ_TYPE_LABELS, raw);
}

/** 库存流水来源单据中文。 */
export function txnDocTypeLabel(raw: string | null | undefined): string {
  return labelOf(TXN_DOC_TYPE_LABELS, raw);
}

/** 凭证来源中文。 */
export function voucherSourceTypeLabel(raw: string | null | undefined): string {
  return labelOf(VOUCHER_SOURCE_TYPE_LABELS, raw);
}

/** 序列号来源中文。 */
export function serialSourceTypeLabel(raw: string | null | undefined): string {
  return labelOf(SERIAL_SOURCE_TYPE_LABELS, raw);
}

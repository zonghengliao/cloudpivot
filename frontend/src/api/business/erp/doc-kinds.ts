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
  SALE_ORDER: 'biz_sales_order',
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
} as const;

/** 支持联动作废的业务对象（`/cancel-impact`、`/cancel-cascade` 的可选项）。 */
export type CancelDocType = keyof typeof DOC_KIND_ROUTES;

/**
 * 能挂业务附件的业务对象（`biz_doc_attachment.doc_type` 的 5 类）。
 *
 * 与后端 `doc_kind::ATTACHMENT_DOC_KINDS` + DDL 的 `ck_biz_doc_attachment_type`
 * 逐字一致（后端单测 `attachment_doc_types_match_rust` 会比对顺序与取值）。
 */
export const ATTACHMENT_DOC_TYPES = [
  'CONTRACT',
  'PURCHASE_ORDER',
  'STOCK_IN',
  'PAYABLE',
  'PAYMENT',
  'FIXED_ASSET',
  'EXPENSE_REPORT',
] as const;

/** 能挂业务附件的业务对象（联合类型）。 */
export type AttachmentDocType = (typeof ATTACHMENT_DOC_TYPES)[number];

// ==================== ② 审批动作 ====================

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
] as const;

/** 审批业务类型（联合类型）。 */
export type FlowBizType = (typeof FLOW_BIZ_TYPES)[number];

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
};

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

/** 收货单子类型（`biz_stock_in.in_type`，与 `ck_biz_si_type` 一致）。 */
export const IN_TYPES = ['PURCHASE', 'RETURN', 'OTHER'] as const;
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

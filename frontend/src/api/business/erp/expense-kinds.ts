/**
 * 费用报销的词表契约（**纯常量，不 import 任何请求层**）。
 *
 * # 为什么单独成文件
 *
 * 与 `asset-kinds.ts` / `doc-kinds.ts` 同款理由：词表要能被**纯逻辑单测**引用。
 * `expense.ts` 里有 `import { del, get, post, put } from '@/api/request'`，
 * 而 `request.ts` 顶层读 `import.meta.env` —— 在 node 里跑 `tsx --test` 会直接崩。
 * 把词表切出来后，`expense-logic.ts` 只依赖这个文件，单测就能零依赖跑。
 *
 * # 取值来源（改这里之前先改后端）
 *
 * - 状态：`backend/src/business/erp/expense_dto.rs::STATUS_*`
 * - 付款状态：同文件 `PAY_*`
 * - 发票类型：迁移 `V0242` 的 CHECK `ck_biz_exp_item_inv`
 * - 付款方式：`expense_dto::PAY_METHODS`（应用层收口，DDL 不设 CHECK）
 *
 * 加值会同时踩后端 `biz_type_domain` 巡检，前端不要自行发明取值。
 */

/** 报销单状态（`expense_dto::STATUS_*`）。 */
export type ExpenseStatus = 'DRAFT' | 'SUBMITTED' | 'APPROVED' | 'PAID' | 'CANCELLED';

/** 付款状态（`expense_dto::PAY_*`）。 */
export type ExpensePayStatus = 'UNPAID' | 'PARTIAL' | 'PAID';

/** 发票类型（与 DDL `ck_biz_exp_item_inv` 逐字一致）。 */
export type ExpenseInvoiceType = 'VAT_SPECIAL' | 'VAT_NORMAL' | 'E_INVOICE' | 'NONE';

/** 付款方式（应用层收口，DDL 不设 CHECK）。 */
export type ExpensePayMethod = 'BANK_TRANSFER' | 'CASH' | 'OFFSET';

/** 状态取值域（顺序同状态机推进顺序）。 */
export const EXPENSE_STATUSES: ExpenseStatus[] = ['DRAFT', 'SUBMITTED', 'APPROVED', 'PAID', 'CANCELLED'];

/** 状态中文名（与后端 `status_label` 逐字一致；后端也下发 `status_label`，这里只做兜底）。 */
export const EXPENSE_STATUS_LABELS: Record<ExpenseStatus, string> = {
  DRAFT: '草稿',
  SUBMITTED: '审批中',
  APPROVED: '已审批（待付款）',
  PAID: '已付清',
  CANCELLED: '已作废',
};

/** 付款状态取值域。 */
export const EXPENSE_PAY_STATUSES: ExpensePayStatus[] = ['UNPAID', 'PARTIAL', 'PAID'];

/** 付款状态中文名。 */
export const EXPENSE_PAY_STATUS_LABELS: Record<ExpensePayStatus, string> = {
  UNPAID: '未付款',
  PARTIAL: '部分付款',
  PAID: '已付清',
};

/** 发票类型取值域。 */
export const EXPENSE_INVOICE_TYPES: ExpenseInvoiceType[] = [
  'VAT_SPECIAL',
  'VAT_NORMAL',
  'E_INVOICE',
  'NONE',
];

/** 发票类型中文名。 */
export const EXPENSE_INVOICE_TYPE_LABELS: Record<ExpenseInvoiceType, string> = {
  VAT_SPECIAL: '增值税专用发票',
  VAT_NORMAL: '增值税普通发票',
  E_INVOICE: '电子发票',
  NONE: '无发票',
};

/** 付款方式取值域（缺省银行转账）。 */
export const EXPENSE_PAY_METHODS: ExpensePayMethod[] = ['BANK_TRANSFER', 'CASH', 'OFFSET'];

/** 付款方式中文名。 */
export const EXPENSE_PAY_METHOD_LABELS: Record<ExpensePayMethod, string> = {
  BANK_TRANSFER: '银行转账',
  CASH: '现金',
  OFFSET: '冲抵',
};

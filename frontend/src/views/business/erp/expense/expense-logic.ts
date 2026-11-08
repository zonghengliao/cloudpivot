/**
 * 费用报销三页的纯逻辑层（我的报销 / 待付款 / 费用类型）。
 *
 * 为什么不写在 `.vue` 里：三页里真正会出错的是「口径」——
 * 含税怎么拆、合计怎么算、哪些状态能提交 / 撤回 / 作废 / 付款、
 * 招待费为什么税额必须是 0。这些 `vue-tsc` 抓不到、页面渲染也看不出来，
 * 只有用 node 跑纯函数才能逐条钉死（`tests/unit/fin3-expense.test.ts`）。
 *
 * # 价税拆分口径（必须与后端 `expense_compute::split` 逐位一致）
 *
 * ```text
 * 不可抵扣（招待费 / 福利费 / 罚款）→ tax = 0，amount = with_tax
 * 可抵扣且 rate > 0              → base = round2(with_tax / (1 + rate))
 *                                 tax  = with_tax − base        ← 差额，不是独立舍入
 *                                 amount = with_tax − tax
 * ```
 *
 * **税额取差额**而不是 `round2(含税 × 税率)`：两次独立舍入会让「分」凭空多一分，
 * 那是月末对不上账的经典成因（后端注释里写得很清楚）。
 *
 * 前端这里全部走**整数分**（`toCents`），只用于输入框旁的实时预览；
 * 落库金额永远以后端返回的 `items[].amount` / `tax_amount` 为准。
 */

import { EXPENSE_PAY_STATUS_LABELS, EXPENSE_STATUS_LABELS } from '@/api/business/erp/expense-kinds';
import type {
  ExpenseInvoiceType,
  ExpensePayMethod,
  ExpenseStatus,
} from '@/api/business/erp/expense-kinds';
import type {
  ExpenseBudgetUsage,
  ExpenseItem,
  ExpenseItemPayload,
  ExpensePayPayload,
  ExpenseReport,
  ExpenseReportPayload,
  ExpenseType,
  ExpenseTypePayload,
} from '@/api/business/erp/expense';
import { formatCents, todayString, toCents } from '@/views/business/erp/shared/money';
import {
  MASTER_CODE_PATTERN,
  masterCodeError,
  toMasterCodeInput,
} from '@/views/business/erp/shared/master-code';

// ==================== 常量 ====================

/** 作废原因最短字数（后端 `MIN_CANCEL_REASON_LEN`，<10 直接 400）。 */
export const MIN_CANCEL_REASON_LEN = 10;

/** 事由长度上限（`biz_expense_report.title VARCHAR(200)`）。 */
export const MAX_TITLE_LEN = 200;

/** 行摘要长度上限（`biz_expense_item.summary VARCHAR(255)`）。 */
export const MAX_SUMMARY_LEN = 255;

/** 默认付款账户科目编码（后端缺省 `1002` 银行存款）。 */
export const DEFAULT_PAY_ACCOUNT = '1002';

/** 不可抵扣类型在税额输入框旁的提示（ARCH-6 §4.10 细节 2 原文）。 */
export const NON_DEDUCTIBLE_HINT = '招待费进项税不得抵扣';

/** 常用税率（后端只校验 0~1，下拉是「帮用户别填成 6」）。 */
export const TAX_RATE_OPTIONS: Array<{ label: string; value: string }> = [
  { label: '0%（不开票 / 免税）', value: '0' },
  { label: '1%', value: '0.01' },
  { label: '3%', value: '0.03' },
  { label: '6%（服务 / 差旅）', value: '0.06' },
  { label: '9%（运输 / 租赁）', value: '0.09' },
  { label: '13%（货物）', value: '0.13' },
];

/** 状态筛选下拉（列表页用；空值 = 全部）。 */
export const STATUS_FILTER_OPTIONS: Array<{ label: string; value: string }> = [
  { label: '草稿', value: 'DRAFT' },
  { label: '审批中', value: 'SUBMITTED' },
  { label: '已审批（待付款）', value: 'APPROVED' },
  { label: '已付清', value: 'PAID' },
  { label: '已作废', value: 'CANCELLED' },
];

// ==================== 价税拆分（口径同后端） ====================

/** 一行明细的价税拆分结果（**整数分**）。 */
export interface SplitPreview {
  /** 含税金额（分） */
  withTaxCents: number;
  /** 不含税金额（分） */
  amountCents: number;
  /** 税额（分） */
  taxCents: number;
}

/**
 * 按「类型是否可抵扣」拆一行明细的价税（预览用，口径同 `expense_compute::split`）。
 *
 * - `deductible === false`（招待费等）→ `tax = 0`，`amount = withTax`；
 * - `rate <= 0` → 同理（0% 不开票）；
 * - 否则 `amount = round2(withTax / (1 + rate))`、`tax = withTax − amount`。
 */
export function splitPreview(
  withTax: string | number | null | undefined,
  rate: string | number | null | undefined,
  deductible: boolean,
): SplitPreview {
  const withTaxCents = toCents(withTax);
  const rateNum = Number(rate ?? 0);
  if (!deductible || !Number.isFinite(rateNum) || rateNum <= 0) {
    return { withTaxCents, amountCents: withTaxCents, taxCents: 0 };
  }
  // 后端：base = round2(with_tax / (1 + rate))；四舍五入到「分」
  const amountCents = Math.round(withTaxCents / (1 + rateNum));
  return {
    withTaxCents,
    amountCents,
    taxCents: withTaxCents - amountCents,
  };
}

/** 一单的合计（整数分）。 */
export interface ExpenseTotals {
  /** 不含税合计（分） */
  amountCents: number;
  /** 税额合计（分） */
  taxCents: number;
  /** 价税合计（分） */
  withTaxCents: number;
  /** 明细条数 */
  itemCount: number;
}

/** 汇总若干条拆分结果（后端 `Summary::of` 同口径）。 */
export function sumSplits(splits: readonly SplitPreview[]): ExpenseTotals {
  const totals: ExpenseTotals = { amountCents: 0, taxCents: 0, withTaxCents: 0, itemCount: 0 };
  for (const split of splits) {
    totals.amountCents += split.amountCents;
    totals.taxCents += split.taxCents;
    totals.withTaxCents += split.withTaxCents;
    totals.itemCount += 1;
  }
  return totals;
}

/** 明细金额展示：`1,234.56` */
export function centsText(cents: number): string {
  return formatCents(cents);
}

/** 税率 `"0.0600"` → `6%`（展示用；非数字原样返回） */
export function rateText(rate: string | number | null | undefined): string {
  const num = Number(rate ?? 0);
  if (!Number.isFinite(num)) {
    return String(rate ?? '—');
  }
  const percent = num * 100;
  return `${Number.isInteger(percent) ? percent : percent.toFixed(2)}%`;
}

// ==================== 状态与动作可用性 ====================

/** 状态配色（TDesign tag theme）。 */
export function statusTheme(status: string | null | undefined): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  switch (status) {
    case 'DRAFT':
      return 'default';
    case 'SUBMITTED':
      return 'warning';
    case 'APPROVED':
      return 'primary';
    case 'PAID':
      return 'success';
    case 'CANCELLED':
      return 'danger';
    default:
      return 'default';
  }
}

/** 付款状态配色。 */
export function payStatusTheme(status: string | null | undefined): 'default' | 'warning' | 'success' {
  switch (status) {
    case 'UNPAID':
      return 'default';
    case 'PARTIAL':
      return 'warning';
    case 'PAID':
      return 'success';
    default:
      return 'default';
  }
}

/** 状态中文（后端下发了 `status_label` 就用它，这里只做兜底）。 */
export function statusText(report: Pick<ExpenseReport, 'status' | 'status_label'>): string {
  return report.status_label || EXPENSE_STATUS_LABELS[report.status as ExpenseStatus] || report.status;
}

/** 付款状态中文。 */
export function payStatusText(report: Pick<ExpenseReport, 'pay_status' | 'pay_status_label'>): string {
  return (
    report.pay_status_label ||
    EXPENSE_PAY_STATUS_LABELS[report.pay_status as keyof typeof EXPENSE_PAY_STATUS_LABELS] ||
    report.pay_status
  );
}

/** 只有草稿能改（改单会重算价税与预算占用）。 */
export function canEditReport(report: Pick<ExpenseReport, 'status'>): boolean {
  return report.status === 'DRAFT';
}

/** 只有草稿能删。 */
export function canDeleteReport(report: Pick<ExpenseReport, 'status'>): boolean {
  return report.status === 'DRAFT';
}

/** 只有草稿能提交。 */
export function canSubmitReport(report: Pick<ExpenseReport, 'status'>): boolean {
  return report.status === 'DRAFT';
}

/** 只有审批中能撤回（`SUBMITTED → DRAFT`）。 */
export function canWithdrawReport(report: Pick<ExpenseReport, 'status'>): boolean {
  return report.status === 'SUBMITTED';
}

/**
 * 作废可用性：`PAID` 后不许作废；已付过一分钱也不许作废（要先冲付款）。
 * 后端同口径（409），前端先拦一道，别让用户点了才看到报错。
 */
export function canCancelReport(
  report: Pick<ExpenseReport, 'status' | 'paid_amount'>,
): boolean {
  if (report.status === 'PAID' || report.status === 'CANCELLED') {
    return false;
  }
  return toCents(report.paid_amount) === 0;
}

/** 付款可用性：只有 `APPROVED`（含部分付款后的 APPROVED）且还有未付余额才可付。 */
export function canPayReport(
  report: Pick<ExpenseReport, 'status' | 'unpaid_amount'>,
): boolean {
  return report.status === 'APPROVED' && toCents(report.unpaid_amount) > 0;
}

/** 作废原因校验（`null` = 通过；后端 <10 字直接 400）。 */
export function cancelReasonError(reason: string): string | null {
  const text = (reason ?? '').trim();
  if (!text) {
    return '作废原因必填（要能追溯到为什么）';
  }
  if ([...text].length < MIN_CANCEL_REASON_LEN) {
    return `作废原因至少 ${MIN_CANCEL_REASON_LEN} 个字（当前 ${[...text].length} 个字）`;
  }
  return null;
}

// ==================== 表单状态 ====================

/** 一条明细的表单态（金额与税率都是**输入框里的字符串**）。 */
export interface ExpenseItemForm {
  /** 本地行 key（`t-table` / `v-for` 用，不进 payload） */
  key: string;
  /** 费用类型 id */
  expense_type_id: string;
  /** 含税金额（字符串，原样提交给后端） */
  with_tax_amount: string;
  /** 税率（字符串，`0.06`） */
  tax_rate: string;
  /** 发票号 */
  invoice_no: string;
  /** 开票日期 */
  invoice_date: string;
  /** 发票类型（空串 = 不填） */
  invoice_type: ExpenseInvoiceType | '';
  /** 行摘要 */
  summary: string;
  /** 行备注 */
  remark: string;
}

/** 报销单表单态。 */
export interface ExpenseForm {
  /** 事由 */
  title: string;
  /** 费用发生日 */
  expense_date: string;
  /** 报销人（空串 = 后端取当前用户） */
  reporter_id: string;
  /** 部门（空串 = 后端取报销人部门） */
  dept_id: string;
  /** 备注 */
  remark: string;
  /** 明细 */
  items: ExpenseItemForm[];
}

/** 表单校验结果。 */
export interface ExpenseFormErrors {
  /** 单头字段错误（key 同 `ExpenseForm`） */
  [field: string]: string;
}

/** 生成一个本地行 key（不用 `Math.random` 便于测试断言前缀） */
let itemKeySeq = 0;
function nextItemKey(): string {
  itemKeySeq += 1;
  return `item-${Date.now().toString(36)}-${itemKeySeq}`;
}

/** 空明细行。 */
export function emptyItemForm(): ExpenseItemForm {
  return {
    key: nextItemKey(),
    expense_type_id: '',
    with_tax_amount: '',
    tax_rate: '0',
    invoice_no: '',
    invoice_date: '',
    invoice_type: '',
    summary: '',
    remark: '',
  };
}

/** 空表单（新建）。 */
export function emptyExpenseForm(): ExpenseForm {
  return {
    title: '',
    expense_date: todayString(),
    reporter_id: '',
    dept_id: '',
    remark: '',
    items: [emptyItemForm()],
  };
}

/** 详情 → 表单（编辑草稿）。 */
export function formFromDetail(detail: {
  report: ExpenseReport;
  items: ExpenseItem[];
}): ExpenseForm {
  return {
    title: detail.report.title,
    expense_date: detail.report.expense_date,
    reporter_id: detail.report.reporter_id,
    dept_id: detail.report.dept_id,
    remark: detail.report.remark ?? '',
    items: detail.items.map((item) => ({
      key: nextItemKey(),
      expense_type_id: item.expense_type_id,
      // 编辑时把后端拆好的含税金额填回去（用户只录含税）
      with_tax_amount: item.with_tax_amount,
      tax_rate: String(Number(item.tax_rate ?? 0)),
      invoice_no: item.invoice_no ?? '',
      invoice_date: item.invoice_date ?? '',
      invoice_type: (item.invoice_type ?? '') as ExpenseInvoiceType | '',
      summary: item.summary,
      remark: item.remark ?? '',
    })),
  };
}

/** 类型是否可抵扣（找不到类型时按「可抵扣」处理，交由后端最终裁决）。 */
export function typeDeductible(
  typeId: string,
  types: readonly Pick<ExpenseType, 'id' | 'tax_deductible'>[],
): boolean {
  const found = types.find((item) => item.id === typeId);
  return found ? found.tax_deductible : true;
}

/** 一行明细的预览（行内编辑表格下方那三列）。 */
export function previewItem(
  item: Pick<ExpenseItemForm, 'expense_type_id' | 'with_tax_amount' | 'tax_rate'>,
  types: readonly Pick<ExpenseType, 'id' | 'tax_deductible'>[],
): SplitPreview {
  return splitPreview(item.with_tax_amount, item.tax_rate, typeDeductible(item.expense_type_id, types));
}

/** 整单合计（实时预览）。 */
export function previewTotals(
  form: Pick<ExpenseForm, 'items'>,
  types: readonly Pick<ExpenseType, 'id' | 'tax_deductible'>[],
): ExpenseTotals {
  return sumSplits(form.items.map((item) => previewItem(item, types)));
}

/**
 * 校验表单（镜像后端 `validate_report` 的 400 文案）。
 *
 * 返回空对象 = 通过；key 为 `items.<index>.<field>` 时页面标红到具体行。
 */
export function validateExpenseForm(form: ExpenseForm, types: readonly ExpenseType[]): ExpenseFormErrors {
  const errors: ExpenseFormErrors = {};
  const title = (form.title ?? '').trim();
  if (!title) {
    errors.title = '事由必填（会进凭证摘要）';
  } else if ([...title].length > MAX_TITLE_LEN) {
    errors.title = `事由不能超过 ${MAX_TITLE_LEN} 个字`;
  }
  if (!form.items.length) {
    errors.items = '报销单至少要一条明细（没有明细的报销单在会计上无意义）';
  }
  form.items.forEach((item, index) => {
    const no = index + 1;
    const type = types.find((candidate) => candidate.id === item.expense_type_id);
    if (!item.expense_type_id) {
      errors[`items.${index}.expense_type_id`] = `第 ${no} 行请选择费用类型`;
    } else if (!type) {
      errors[`items.${index}.expense_type_id`] = `第 ${no} 行的费用类型已停用或不存在，请重选`;
    }
    const amountCents = toCents(item.with_tax_amount);
    if (!String(item.with_tax_amount ?? '').trim()) {
      errors[`items.${index}.with_tax_amount`] = `第 ${no} 行请填含税金额`;
    } else if (amountCents <= 0) {
      errors[`items.${index}.with_tax_amount`] = `第 ${no} 行含税金额必须大于 0`;
    } else if (type && toCents(type.amount_limit) > 0 && amountCents > toCents(type.amount_limit)) {
      errors[`items.${index}.with_tax_amount`] =
        `第 ${no} 行超过「${type.type_name}」单笔上限 ${formatCents(toCents(type.amount_limit))}`;
    }
    const rate = Number(item.tax_rate ?? 0);
    if (!Number.isFinite(rate) || rate < 0 || rate > 1) {
      errors[`items.${index}.tax_rate`] = `第 ${no} 行税率必须在 0~1 之间（6% 请填 0.06）`;
    }
    if (!(item.summary ?? '').trim()) {
      errors[`items.${index}.summary`] = `第 ${no} 行摘要必填`;
    }
    if (type?.need_invoice_no && !(item.invoice_no ?? '').trim()) {
      errors[`items.${index}.invoice_no`] = `第 ${no} 行「${type.type_name}」必须填发票号`;
    }
  });
  return errors;
}

/** 校验结果 → 第一条可展示的错误（页面顶部提示）。 */
export function firstError(errors: ExpenseFormErrors): string {
  const keys = Object.keys(errors);
  return keys.length ? errors[keys[0] as string] ?? '' : '';
}

/** 表单 → 建单 / 改单 payload（**只提交含税**，价税由后端拆）。 */
export function toReportPayload(form: ExpenseForm): ExpenseReportPayload {
  return {
    title: form.title.trim(),
    expense_date: form.expense_date || undefined,
    // 空串一律不发：后端把 `reporter_id: ""` 当「显式给空」会走 UUID 解析并 400
    reporter_id: form.reporter_id.trim() || undefined,
    dept_id: form.dept_id.trim() || undefined,
    remark: form.remark.trim() || undefined,
    items: form.items.map((item): ExpenseItemPayload => ({
      expense_type_id: item.expense_type_id,
      with_tax_amount: String(item.with_tax_amount).trim(),
      tax_rate: String(item.tax_rate ?? 0).trim() || '0',
      invoice_no: item.invoice_no.trim() || undefined,
      invoice_date: item.invoice_date || undefined,
      invoice_type: item.invoice_type || undefined,
      summary: item.summary.trim(),
      remark: item.remark.trim() || undefined,
    })),
  };
}

// ==================== 付款 ====================

/** 付款表单态。 */
export interface ExpensePayForm {
  /** 金额（字符串） */
  amount: string;
  /** 付款日期 */
  pay_date: string;
  /** 付款方式 */
  pay_method: ExpensePayMethod;
  /** 付款账户科目编码 */
  pay_account_code: string;
  /** 备注 */
  remark: string;
}

/** 空付款表单（金额预填**未付**余额，不是报销总额）。 */
export function emptyPayForm(report: Pick<ExpenseReport, 'unpaid_amount'>): ExpensePayForm {
  return {
    amount: (toCents(report.unpaid_amount) / 100).toFixed(2),
    pay_date: todayString(),
    pay_method: 'BANK_TRANSFER',
    pay_account_code: DEFAULT_PAY_ACCOUNT,
    remark: '',
  };
}

/** 校验付款（镜像后端 `validate_pay`）。 */
export function validatePayForm(form: ExpensePayForm, unpaid: string | number): string | null {
  const amountCents = toCents(form.amount);
  if (!String(form.amount ?? '').trim()) {
    return '付款金额必填';
  }
  if (amountCents <= 0) {
    return '付款金额必须大于 0（冲销已有付款请走「作废」整单红冲）';
  }
  if (amountCents > toCents(unpaid)) {
    return `本次付款不能超过未付金额 ${formatCents(toCents(unpaid))}`;
  }
  if (!form.pay_date) {
    return '付款日期必填';
  }
  return null;
}

/** 付款表单 → payload。 */
export function toPayPayload(form: ExpensePayForm): ExpensePayPayload {
  return {
    amount: String(form.amount).trim(),
    pay_date: form.pay_date || undefined,
    pay_method: form.pay_method,
    pay_account_code: form.pay_account_code.trim() || undefined,
    remark: form.remark.trim() || undefined,
  };
}

// ==================== 费用类型 ====================

/** 费用类型表单态。 */
export interface ExpenseTypeForm {
  /** 编码 */
  type_code: string;
  /** 名称 */
  type_name: string;
  /** 借方科目 id */
  account_id: string;
  /** 可抵扣 */
  tax_deductible: boolean;
  /** 受预算控制 */
  budget_controlled: boolean;
  /** 必填发票号 */
  need_invoice_no: boolean;
  /** 单笔上限（`0` = 不限） */
  amount_limit: string;
  /** 排序 */
  sort_order: number;
  /** 状态 */
  status: number;
  /** 备注 */
  remark: string;
}

/**
 * 费用类型编码字符集（镜像后端 `seq::validate_manual_code`：字母 / 数字 / `-` `_` `.`，≤ 40）。
 *
 * CODE-07 修正：旧值是 `^[A-Z][A-Z0-9_]*$` —— **不接受连字符**，而取号器产出的
 * `ET-01` 正带连字符，用户点完「生成编码」会被前端自己的校验拦住。现在统一指向
 * [`MASTER_CODE_PATTERN`]，与六个主数据表单同一口径。
 */
export const TYPE_CODE_PATTERN = MASTER_CODE_PATTERN;

/** 空费用类型表单。 */
export function emptyTypeForm(): ExpenseTypeForm {
  return {
    type_code: '',
    type_name: '',
    account_id: '',
    tax_deductible: true,
    budget_controlled: true,
    need_invoice_no: true,
    amount_limit: '0',
    sort_order: 0,
    status: 1,
    remark: '',
  };
}

/** 列表行 → 表单（编辑）。 */
export function typeFormFromRow(row: ExpenseType): ExpenseTypeForm {
  return {
    type_code: row.type_code,
    type_name: row.type_name,
    account_id: row.account_id,
    tax_deductible: row.tax_deductible,
    budget_controlled: row.budget_controlled,
    need_invoice_no: row.need_invoice_no,
    amount_limit: String(Number(row.amount_limit ?? 0)),
    sort_order: row.sort_order ?? 0,
    status: row.status,
    remark: row.remark ?? '',
  };
}

/** 校验费用类型（镜像后端 `validate_type`）。返回 `null` = 通过。 */
export function validateTypeForm(form: ExpenseTypeForm): string | null {
  // CODE-07：编码**可以留空** —— 留空由后端自动取号（`ET-01` 形态，2 位纯流水、
  // 不计日期、永不重置），「生成编码」按钮只是提前看号。
  // 填了则按后端字符集校验（字母 / 数字 / `-` `_` `.`，≤ 40；不要求字母开头）。
  const codeError = masterCodeError(form.type_code);
  if (codeError) {
    return codeError;
  }
  if (!(form.type_name ?? '').trim()) {
    return '费用类型名称必填';
  }
  if (!form.account_id) {
    return '借方费用科目必填（末级科目）';
  }
  if (toCents(form.amount_limit) < 0) {
    return '单笔金额上限不能为负（0 表示不限）';
  }
  return null;
}

/** 费用类型表单 → payload。 */
export function toTypePayload(form: ExpenseTypeForm): ExpenseTypePayload {
  return {
    // CODE-07：留空发 `null`（不是 `''`）—— 后端 `resolve_manual_code` 两种都当没填，
    // 但 `null` 一眼能看出「没填」而不是「填了个空串」。
    type_code: toMasterCodeInput(form.type_code),
    type_name: form.type_name.trim(),
    account_id: form.account_id,
    tax_deductible: form.tax_deductible,
    budget_controlled: form.budget_controlled,
    need_invoice_no: form.need_invoice_no,
    amount_limit: String(form.amount_limit).trim() || '0',
    sort_order: Number(form.sort_order) || 0,
    status: Number(form.status) || 0,
    remark: form.remark.trim() || undefined,
  };
}

// ==================== 详情页展示 ====================

/** 预算占用区块的一行（科目名 / 部门名由页面用字典补齐，这里只做归一）。 */
export interface BudgetUsageRow {
  /** 占用 id */
  id: string;
  /** 部门 */
  dept_name: string;
  /** 科目（编码 + 名称，缺名称时只有编码） */
  account_text: string;
  /** 占用多少（含税） */
  amount: string;
  /** 占哪个月 */
  period_text: string;
  /** 状态文案 */
  status_text: string;
  /** 是否超预算 */
  over_budget: boolean;
  /** 超支额（`null` = 当时没超支） */
  over_amount: string | null;
  /** 释放原因 */
  release_reason: string | null;
}

/** 预算占用状态中文（后端 `biz_expense_budget_usage.status`：OCCUPIED / CONSUMED / RELEASED）。 */
export function usageStatusText(status: string | null | undefined): string {
  switch (status) {
    case 'OCCUPIED':
      return '占用中';
    // 审批通过入账后占用转「已消耗」（`expense_bridge::on_expense_settled`），
    // 不是「还在占用」也不是「已释放」—— 少这一档页面会把枚举原样吐给财务（实测踩过）
    case 'CONSUMED':
      return '已消耗';
    case 'RELEASED':
      return '已释放';
    default:
      return status ?? '—';
  }
}

/**
 * 预算占用 → 展示行（§4.10 细节 4：部门 / 科目 / 占多少 / 占哪个月）。
 *
 * `deptName` / `accountName` 由页面从字典注入：`UsageRes` 只下发 id + 科目编码，
 * 不在前端猜名字（猜错比留空更糟）。
 */
export function budgetUsageRows(
  usages: readonly ExpenseBudgetUsage[],
  deptName: (id: string) => string,
  accountName: (id: string) => string,
): BudgetUsageRow[] {
  return usages.map((usage) => {
    const account = accountName(usage.account_id);
    return {
      id: usage.id,
      dept_name: deptName(usage.dept_id) || '—',
      account_text: account ? `${usage.account_code} ${account}` : usage.account_code,
      amount: usage.amount,
      period_text: usage.period_code,
      status_text: usageStatusText(usage.status),
      over_budget: usage.over_budget,
      over_amount: usage.over_amount,
      release_reason: usage.release_reason,
    };
  });
}

/**
 * 列表页「预算」列的标记（FIX-5）：**真超支**标红「超预算」，**未编预算**标黄「未编预算」，
 * 两者都 false → 返回 `null`（正常，页面显示 `—`）。
 *
 * 为什么必须分开：修复前「没编预算」的单据也落 `over_budget=1 / over_amount=全额`，
 * 列表页一片红「超预算」，业务分不清「真花超了」和「压根没编预算」。
 */
export function budgetFlag(
  row: Pick<ExpenseReport, 'over_budget' | 'missing_budget'>,
): { text: string; theme: 'danger' | 'warning' } | null {
  if (row.over_budget) {
    return { text: '超预算', theme: 'danger' };
  }
  if (row.missing_budget) {
    return { text: '未编预算', theme: 'warning' };
  }
  return null;
}

/** 付款记录的状态配色（付款行状态：PENDING / APPROVED / REJECTED / CANCELLED …）。 */
export function paymentStatusTheme(status: string | null | undefined): 'default' | 'warning' | 'success' | 'danger' {
  switch (status) {
    case 'PENDING':
      return 'warning';
    case 'APPROVED':
    case 'PAID':
      return 'success';
    case 'REJECTED':
    case 'CANCELLED':
      return 'danger';
    default:
      return 'default';
  }
}

/** 付款记录状态中文。 */
export function paymentStatusText(status: string | null | undefined): string {
  switch (status) {
    case 'PENDING':
      return '审批中';
    case 'APPROVED':
      return '已付款';
    case 'REJECTED':
      return '已驳回';
    case 'CANCELLED':
      return '已作废';
    default:
      return status ?? '—';
  }
}

/** 发票类型中文（取值域只来自后端 `INVOICE_TYPES`；未知值原样显示以暴露脏数据）。 */
export function invoiceTypeText(raw: string | null | undefined): string {
  switch (raw) {
    case 'VAT_SPECIAL':
      return '增值税专用发票';
    case 'VAT_NORMAL':
      return '增值税普通发票';
    case 'E_INVOICE':
      return '电子发票';
    case 'NONE':
      return '无发票';
    default:
      return raw ?? '';
  }
}

/** 付款方式中文（`PAY_METHODS`）。 */
export function payMethodText(raw: string | null | undefined): string {
  switch (raw) {
    case 'BANK_TRANSFER':
      return '银行转账';
    case 'CASH':
      return '现金';
    case 'OFFSET':
      return '冲抵';
    default:
      return raw ?? '—';
  }
}

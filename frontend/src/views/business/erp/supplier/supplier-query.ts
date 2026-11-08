/**
 * 供应商页纯逻辑（P4-A）——所有可测的判断都在这里，`.vue` 只负责渲染。
 *
 * # 字段口径（硬红线①）
 *
 * 出参逐字抄 `backend/src/business/erp/dto.rs::SupplierRes`（= `biz_supplier` 的列），
 * 并用真接口 `GET /api/business/erp/supplier/page` 核对过：
 * `supplier_code / supplier_name / short_name / contact_person / contact_phone / email /
 *  tax_no / address / credit_days / credit_limit / payable_account_id / invoice_title /
 *  invoice_bank_name / invoice_bank_account / status / remark`。
 *
 * 猜成 `code` / `name` 的代价是整列空白、表单回填空白，编译期抓不到（W4b-1 的教训）。
 */
import type { PrimaryTableCol } from 'tdesign-vue-next';
import type { CreateSupplierReq, Supplier, UpdateSupplierReq } from '@/api/business/erp';

/** 每页条数 */
export const SUPPLIER_PAGE_SIZE = 20;

/** 状态筛选（1 启用 / 0 停用；'' = 全部） */
export const SUPPLIER_STATUS_OPTIONS: { label: string; value: number }[] = [
  { label: '启用', value: 1 },
  { label: '停用', value: 0 },
];

/** 筛选行 */
export interface SupplierFilterState {
  /** 关键词（编码 / 名称 / 简称模糊） */
  keyword: string;
  /** 状态：1 / 0 / ''（全部） */
  status: number | '';
}

/** 空筛选 */
export function emptySupplierFilter(): SupplierFilterState {
  return { keyword: '', status: '' };
}

/** 是否处于筛选态 */
export function isSupplierFiltered(filter: SupplierFilterState): boolean {
  return Boolean(filter.keyword.trim()) || filter.status !== '';
}

/** `/erp/supplier/page` 的 query */
export interface SupplierQuery {
  /** 关键词；空串不发 */
  keyword?: string;
  /** 状态；'' 不发 */
  status?: number;
  /** 页码 */
  page: number;
  /** 每页条数 */
  page_size: number;
}

/** 拼分页入参（空串一律丢掉：`?keyword=` 会被后端当「等于空串」过滤，查出 0 行） */
export function buildSupplierQuery(
  filter: SupplierFilterState,
  page: number,
  pageSize: number = SUPPLIER_PAGE_SIZE,
): SupplierQuery {
  const query: SupplierQuery = { page, page_size: pageSize };
  const keyword = filter.keyword.trim();
  if (keyword) {
    query.keyword = keyword;
  }
  if (filter.status !== '') {
    query.status = filter.status;
  }
  return query;
}

/** 状态文案 */
export function supplierStatusLabel(status: number): string {
  if (status === 1) {
    return '启用';
  }
  if (status === 0) {
    return '停用';
  }
  return `未知(${status})`;
}

/** 状态 → t-tag 主题：启用绿、停用灰、脏值也灰 */
export function supplierStatusTheme(status: number): 'success' | 'default' {
  return status === 1 ? 'success' : 'default';
}

/** 空值统一显示 `—`（`null` / `''` / 全空格都算空） */
export function textOr(text: string | null | undefined): string {
  const value = (text ?? '').trim();
  return value === '' ? '—' : value;
}

/** 联系人 + 电话合并成一列（主数据列表最常见的看法：找谁、打哪个号） */
export function contactLabel(row: Pick<Supplier, 'contact_person' | 'contact_phone'>): string {
  const person = (row.contact_person ?? '').trim();
  const phone = (row.contact_phone ?? '').trim();
  if (person && phone) {
    return `${person} · ${phone}`;
  }
  if (person) {
    return person;
  }
  return phone ? `— · ${phone}` : '—';
}

/**
 * 金额/额度格式化：`"1000.0000"` → `"1000"`、`"1500.5000"` → `"1500.5"`。
 *
 * `credit_limit` 是 `Decimal`，后端以字符串下发，这里按字符串裁尾零（不 `parseFloat`）。
 */
export function formatAmount(value: number | string | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '—';
  }
  const text = String(value).trim();
  if (!text.includes('.')) {
    return text;
  }
  const trimmed = text.replace(/0+$/, '').replace(/\.$/, '');
  return trimmed === '' ? '0' : trimmed;
}

/** 账期文案（0 天 = 现结，比「账期 0 天」好读） */
export function creditDaysLabel(days: number | null | undefined): string {
  if (days === null || days === undefined) {
    return '—';
  }
  return days === 0 ? '现结（0 天）' : `${days} 天`;
}

/** 空态判断：`rows.length === 0` 才显示空态 */
export function isSupplierListEmpty(rows: Pick<Supplier, 'id'>[] | null | undefined): boolean {
  return (rows?.length ?? 0) === 0;
}

/** 表格列（colKey 与后端出参同名；`op` 是唯一非后端字段列） */
export const SUPPLIER_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'supplier_code', title: '供应商编码', width: 150, fixed: 'left', ellipsis: true },
  { colKey: 'supplier_name', title: '供应商名称', minWidth: 220, ellipsis: true },
  { colKey: 'contact_person', title: '联系人 / 电话', width: 200, ellipsis: true },
  { colKey: 'tax_no', title: '税号', width: 200, ellipsis: true },
  { colKey: 'credit_days', title: '账期', width: 110 },
  { colKey: 'status', title: '状态', width: 100 },
  { colKey: 'op', title: '操作', width: 170, fixed: 'right' },
];

/** 新建 / 编辑表单（全部按字符串存，提交时把空串转 `null` 表示清空） */
export interface SupplierFormState {
  /** 编码（编辑态只读：后端 `UpdateSupplierReq` 没有这个字段） */
  supplier_code: string;
  /** 名称 */
  supplier_name: string;
  /** 简称 */
  short_name: string;
  /** 联系人 */
  contact_person: string;
  /** 联系电话 */
  contact_phone: string;
  /** 邮箱 */
  email: string;
  /** 纳税人识别号 */
  tax_no: string;
  /** 地址 */
  address: string;
  /** 账期天数 */
  credit_days: number;
  /** 信用额度 */
  credit_limit: number;
  /** 应付科目 id */
  payable_account_id: string;
  /** 开票抬头 */
  invoice_title: string;
  /** 开户行 */
  invoice_bank_name: string;
  /** 银行账号 */
  invoice_bank_account: string;
  /** 启用状态 1 / 0 */
  status: number;
  /** 备注 */
  remark: string;
}

/** 空表单 */
export function emptySupplierForm(): SupplierFormState {
  return {
    supplier_code: '',
    supplier_name: '',
    short_name: '',
    contact_person: '',
    contact_phone: '',
    email: '',
    tax_no: '',
    address: '',
    credit_days: 0,
    credit_limit: 0,
    payable_account_id: '',
    invoice_title: '',
    invoice_bank_name: '',
    invoice_bank_account: '',
    status: 1,
    remark: '',
  };
}

/** 编辑回填 */
export function supplierFormFromRow(row: Supplier): SupplierFormState {
  return {
    supplier_code: row.supplier_code,
    supplier_name: row.supplier_name,
    short_name: row.short_name ?? '',
    contact_person: row.contact_person ?? '',
    contact_phone: row.contact_phone ?? '',
    email: row.email ?? '',
    tax_no: row.tax_no ?? '',
    address: row.address ?? '',
    credit_days: Number(row.credit_days ?? 0),
    credit_limit: Number(row.credit_limit ?? 0),
    payable_account_id: row.payable_account_id ?? '',
    invoice_title: row.invoice_title ?? '',
    invoice_bank_name: row.invoice_bank_name ?? '',
    invoice_bank_account: row.invoice_bank_account ?? '',
    status: row.status === 0 ? 0 : 1,
    remark: row.remark ?? '',
  };
}

/** 表单字段错误 */
export type SupplierFieldErrors = Partial<
  Record<'supplier_code' | 'supplier_name' | 'email' | 'credit_days' | 'credit_limit', string>
>;

/** 是否有字段错误 */
export function hasSupplierFieldError(errors: SupplierFieldErrors): boolean {
  return Object.keys(errors).length > 0;
}

/**
 * 校验（口径照 `dto::CreateSupplierReq::validate`）。
 *
 * 邮箱只做「有 @ 且有点」的粗校验 —— 后端也不做强校验，前端没必要比后端更严。
 */
export function validateSupplierForm(form: SupplierFormState, isEdit: boolean): SupplierFieldErrors {
  const errors: SupplierFieldErrors = {};
  if (!isEdit) {
    // CODE-05：编码**可以留空** —— 留空由后端自动取号（SUP-0001 形态），
    // 「生成编码」按钮只是提前看号。所以这里只校验长度，不再拦「必填」。
    const code = form.supplier_code.trim();
    if (code.length > 40) {
      errors.supplier_code = '供应商编码最多 40 个字符';
    }
  }
  const name = form.supplier_name.trim();
  if (!name) {
    errors.supplier_name = '供应商名称必填';
  } else if (name.length > 200) {
    errors.supplier_name = '供应商名称最多 200 个字符';
  }
  const email = form.email.trim();
  if (email && !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)) {
    errors.email = '邮箱格式不对';
  }
  if (!Number.isFinite(form.credit_days) || form.credit_days < 0) {
    errors.credit_days = '账期天数不能为负';
  }
  if (!Number.isFinite(form.credit_limit) || form.credit_limit < 0) {
    errors.credit_limit = '信用额度不能为负';
  }
  return errors;
}

/** 空串 → `null`（后端把 `null` 当「清空」，`""` 会存成空串） */
function nullable(text: string): string | null {
  const value = text.trim();
  return value === '' ? null : value;
}

/** 新建请求体 */
export function buildSupplierCreatePayload(form: SupplierFormState): CreateSupplierReq {
  return {
    // CODE-05：留空发 `null`（不是 `''`）—— 语义更明确，后端 `resolve_manual_code` 两种都收，
    // 但 `null` 一眼能看出「没填」而不是「填了个空串」。
    supplier_code: nullable(form.supplier_code),
    supplier_name: form.supplier_name.trim(),
    short_name: nullable(form.short_name),
    contact_person: nullable(form.contact_person),
    contact_phone: nullable(form.contact_phone),
    email: nullable(form.email),
    tax_no: nullable(form.tax_no),
    address: nullable(form.address),
    credit_days: Number(form.credit_days) || 0,
    credit_limit: Number(form.credit_limit) || 0,
    payable_account_id: nullable(form.payable_account_id),
    invoice_title: nullable(form.invoice_title),
    invoice_bank_name: nullable(form.invoice_bank_name),
    invoice_bank_account: nullable(form.invoice_bank_account),
    status: form.status === 0 ? 0 : 1,
    remark: nullable(form.remark),
  };
}

/**
 * 编辑请求体：**只发真改过的字段**（缺席 = 不改）。
 *
 * 文本字段从有到无 → 发 `null`（清空），发 `""` 会被后端存成空串，列表里就成了「有值但是空的」。
 */
export function buildSupplierUpdatePayload(form: SupplierFormState, original: Supplier): UpdateSupplierReq {
  const payload: UpdateSupplierReq = {};
  const name = form.supplier_name.trim();
  if (name && name !== original.supplier_name) {
    payload.supplier_name = name;
  }
  const textPairs: [
    'short_name' | 'contact_person' | 'contact_phone' | 'email' | 'tax_no' | 'address'
      | 'payable_account_id' | 'invoice_title' | 'invoice_bank_name' | 'invoice_bank_account' | 'remark',
    string,
    string | null,
  ][] = [
    ['short_name', form.short_name, original.short_name],
    ['contact_person', form.contact_person, original.contact_person],
    ['contact_phone', form.contact_phone, original.contact_phone],
    ['email', form.email, original.email],
    ['tax_no', form.tax_no, original.tax_no],
    ['address', form.address, original.address],
    ['payable_account_id', form.payable_account_id, original.payable_account_id],
    ['invoice_title', form.invoice_title, original.invoice_title],
    ['invoice_bank_name', form.invoice_bank_name, original.invoice_bank_name],
    ['invoice_bank_account', form.invoice_bank_account, original.invoice_bank_account],
    ['remark', form.remark, original.remark],
  ];
  for (const [key, next, previous] of textPairs) {
    const nextValue = nullable(next);
    const previousValue = (previous ?? '').trim() === '' ? null : (previous as string).trim();
    if (nextValue !== previousValue) {
      payload[key] = nextValue;
    }
  }
  const creditDays = Number(form.credit_days) || 0;
  if (creditDays !== Number(original.credit_days ?? 0)) {
    payload.credit_days = creditDays;
  }
  const creditLimit = Number(form.credit_limit) || 0;
  if (creditLimit !== Number(original.credit_limit ?? 0)) {
    payload.credit_limit = creditLimit;
  }
  const status = form.status === 0 ? 0 : 1;
  if (status !== original.status) {
    payload.status = status;
  }
  return payload;
}

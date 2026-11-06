/**
 * 客户档案页纯逻辑（SO-03）——所有可测的判断都在这里，`.vue` 只负责渲染。
 *
 * # 字段口径（硬红线①）
 *
 * 出参逐字抄 `backend/src/business/erp/customer_handler.rs::CustomerRes`
 * （= `biz_customer` 的存活列），并用真接口 `GET /api/business/erp/customer/page`
 * 核对过（`docs/evidence/20261106-so03-customer/`）：
 *
 *     id / code / name / tax_no / contact_name / contact_phone / address /
 *     credit_limit / status / remark / created_at / updated_at
 *
 * ⚠️ 与供应商页**不同名**：供应商是 `supplier_code` / `supplier_name`，
 * 客户就是 `code` / `name`（`V0260__sales_customer.up.sql` 的列名）。照供应商抄
 * 前缀的代价是整列空白 + 表单回填空白，`vue-tsc` 抓不到（W4b-1 的教训）。
 *
 * # 状态是字符串域，不是 1/0
 *
 * `status ∈ {'ACTIVE', 'FROZEN'}`（`ck_biz_customer_status`）。切状态走
 * `PUT /customer/{id}` 的**部分更新**，请求体只有 `{status: 'FROZEN'}` ——
 * 整表单回写会把别人刚改的字段覆盖回旧值。
 *
 * # 信用额度
 *
 * `NUMERIC(18,4)`，后端以**字符串**下发（`"200000.5000"`）。所以：
 * - 列表 / 详情按字符串裁尾零显示（`formatCreditLimit`，不 `parseFloat`）；
 * - 提交时转成字符串（`"200000.5"`），不把 `0.1 + 0.2 = 0.30000000000000004` 发给后端。
 */
import type { PrimaryTableCol } from 'tdesign-vue-next';
import type {
  CreateCustomerReq,
  Customer,
  CustomerStatus,
  UpdateCustomerReq,
} from '@/api/business/erp';

/** 每页条数（`V0261` 页面的默认口径；后端上限 200、`0` → 20） */
export const CUSTOMER_PAGE_SIZE = 20;

/** 状态域（顺序 = 默认展示顺序：正常在前） */
export const CUSTOMER_STATUS_OPTIONS: { label: string; value: CustomerStatus }[] = [
  { label: '正常', value: 'ACTIVE' },
  { label: '冻结', value: 'FROZEN' },
];

/** 字段长度上限（逐字抄 `customer_handler.rs` 的 `check_optional_text` / `VARCHAR(n)`） */
export const CUSTOMER_LIMITS = {
  /** 编码 ≤ 40 */
  code: 40,
  /** 名称 ≤ 200 */
  name: 200,
  /** 税号 ≤ 50 */
  tax_no: 50,
  /** 联系人 ≤ 50 */
  contact_name: 50,
  /** 电话 ≤ 30 */
  contact_phone: 30,
  /** 地址 ≤ 255 */
  address: 255,
  /** 备注 ≤ 2000 */
  remark: 2000,
} as const;

/** 信用额度整数位上限：`NUMERIC(18,4)` → 14 位整数 + 4 位小数 */
export const CUSTOMER_CREDIT_LIMIT_MAX = 99_999_999_999_999.9999;

// ==================== 筛选 ====================

/**
 * 筛选行。
 *
 * `code` / `name` 是**两个独立**入参（后端 `CustomerFilter` 就是两个字段，
 * 各走各的 LIKE），不要合成一个 `keyword` —— 后端没有 `keyword`，发了也不认。
 */
export interface CustomerFilterState {
  /** 客户编码（模糊） */
  code: string;
  /** 客户名称（模糊） */
  name: string;
  /** 状态：`ACTIVE` / `FROZEN` / `''`（全部） */
  status: CustomerStatus | '';
}

/** 空筛选 */
export function emptyCustomerFilter(): CustomerFilterState {
  return { code: '', name: '', status: '' };
}

/** 是否处于筛选态（空态文案要区分「还没数据」和「筛没了」） */
export function isCustomerFiltered(filter: CustomerFilterState): boolean {
  return Boolean(filter.code.trim()) || Boolean(filter.name.trim()) || filter.status !== '';
}

/** `/customer/page` 的 query（空串不发：`?code=` 会被后端当「等于空串」过滤，查出 0 行） */
export interface CustomerQuery {
  /** 编码模糊；空串不发 */
  code?: string;
  /** 名称模糊；空串不发 */
  name?: string;
  /** 状态精确；`''` 不发 */
  status?: CustomerStatus;
  /** 页码（从 1 起） */
  page: number;
  /** 每页条数 */
  page_size: number;
}

/** 拼分页入参 */
export function buildCustomerQuery(
  filter: CustomerFilterState,
  page: number,
  pageSize: number = CUSTOMER_PAGE_SIZE,
): CustomerQuery {
  const query: CustomerQuery = { page, page_size: pageSize };
  const code = filter.code.trim();
  if (code) {
    query.code = code;
  }
  const name = filter.name.trim();
  if (name) {
    query.name = name;
  }
  if (filter.status !== '') {
    query.status = filter.status;
  }
  return query;
}

// ==================== 展示 ====================

/** 状态文案（脏值也要能显示，不让界面出现 `undefined`） */
export function customerStatusLabel(status: string | null | undefined): string {
  if (status === 'ACTIVE') {
    return '正常';
  }
  if (status === 'FROZEN') {
    return '冻结';
  }
  return status ? `未知(${status})` : '—';
}

/** 状态 → `t-tag` 主题：正常绿、冻结橙（冻结是「还能解冻的业务态」，不是灰色停用） */
export function customerStatusTheme(status: string | null | undefined): 'success' | 'warning' | 'default' {
  if (status === 'ACTIVE') {
    return 'success';
  }
  if (status === 'FROZEN') {
    return 'warning';
  }
  return 'default';
}

/** 状态切换的下一个值（按钮文案也用同一个来源，避免两处漂移） */
export function nextCustomerStatus(status: string | null | undefined): CustomerStatus {
  return status === 'FROZEN' ? 'ACTIVE' : 'FROZEN';
}

/** 状态切换按钮文案 */
export function customerToggleLabel(status: string | null | undefined): string {
  return status === 'FROZEN' ? '解冻' : '冻结';
}

/** 空值统一显示 `—`（`null` / `''` / 全空格都算空） */
export function textOr(text: string | null | undefined): string {
  const value = (text ?? '').trim();
  return value === '' ? '—' : value;
}

/** 联系人 + 电话合并成一列（主数据列表最常见的看法：找谁、打哪个号） */
export function contactLabel(
  row: Pick<Customer, 'contact_name' | 'contact_phone'>,
): string {
  const person = (row.contact_name ?? '').trim();
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
 * 信用额度格式化：`"200000.5000"` → `"200000.5"`、`"0.0000"` → `"0"`。
 *
 * 按字符串裁尾零（不 `parseFloat`，避免大额丢精度）。`0` 单独给一句人话 ——
 * 用户看到 `0` 常常以为是「额度用完了」，其实是「不控额度」。
 */
export function formatCreditLimit(value: string | number | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '—';
  }
  const text = String(value).trim();
  if (!text.includes('.')) {
    return text === '0' ? '不控额度' : text;
  }
  const trimmed = text.replace(/0+$/, '').replace(/\.$/, '');
  return trimmed === '' || trimmed === '0' ? '不控额度' : trimmed;
}

/** 空态判断 */
export function isCustomerListEmpty(rows: Pick<Customer, 'id'>[] | null | undefined): boolean {
  return (rows?.length ?? 0) === 0;
}

/** 表格列（colKey 与后端出参同名；`op` 是唯一非后端字段列） */
export const CUSTOMER_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'code', title: '客户编码', width: 150, fixed: 'left', ellipsis: true },
  { colKey: 'name', title: '客户名称', minWidth: 220, ellipsis: true },
  { colKey: 'contact_name', title: '联系人 / 电话', width: 200, ellipsis: true },
  { colKey: 'credit_limit', title: '信用额度', width: 140, align: 'right' },
  { colKey: 'status', title: '状态', width: 100 },
  { colKey: 'op', title: '操作', width: 220, fixed: 'right' },
];

// ==================== 表单 ====================

/**
 * 新建 / 编辑表单。
 *
 * 文本一律按字符串存（`null` 折成 `''`），提交时才折回 `''`/`null`：
 * `t-input` 的 `v-model` 若给 `null` 会在清空后变 `undefined`，再传给后端就丢字段。
 */
export interface CustomerFormState {
  /** 编码（编辑态只读：改编码等于换身份，列表右滑会找不到人） */
  code: string;
  /** 名称 */
  name: string;
  /** 税号 */
  tax_no: string;
  /** 联系人 */
  contact_name: string;
  /** 联系电话 */
  contact_phone: string;
  /** 地址 */
  address: string;
  /** 信用额度（`0` = 不控额度） */
  credit_limit: number;
  /** 状态 */
  status: CustomerStatus;
  /** 备注 */
  remark: string;
}

/** 空表单（新建默认 `ACTIVE`：后端省略 status 也是 ACTIVE，两边必须同口径） */
export function emptyCustomerForm(): CustomerFormState {
  return {
    code: '',
    name: '',
    tax_no: '',
    contact_name: '',
    contact_phone: '',
    address: '',
    credit_limit: 0,
    status: 'ACTIVE',
    remark: '',
  };
}

/** 编辑回填（`customer_code` 那种前缀字段在这里不存在，别照供应商抄） */
export function customerFormFromRow(row: Customer): CustomerFormState {
  return {
    code: row.code,
    name: row.name,
    tax_no: row.tax_no ?? '',
    contact_name: row.contact_name ?? '',
    contact_phone: row.contact_phone ?? '',
    address: row.address ?? '',
    credit_limit: Number(row.credit_limit ?? 0),
    status: row.status === 'FROZEN' ? 'FROZEN' : 'ACTIVE',
    remark: row.remark ?? '',
  };
}

/** 表单字段错误 */
export type CustomerFieldErrors = Partial<
  Record<
    | 'code'
    | 'name'
    | 'tax_no'
    | 'contact_name'
    | 'contact_phone'
    | 'address'
    | 'credit_limit'
    | 'remark',
    string
  >
>;

/** 是否有字段错误 */
export function hasCustomerFieldError(errors: CustomerFieldErrors): boolean {
  return Object.keys(errors).length > 0;
}

/** 字符数（按 code point 数，与后端 `chars().count()` 同口径：emoji 不双计） */
function charCount(text: string): number {
  return Array.from(text.trim()).length;
}

/**
 * 校验（口径照 `customer_handler.rs::CreateCustomerReq::validate` /
 * `UpdateCustomerReq::validate`）。
 *
 * 只在**入口**拦「必填 / 超长 / 额度为负」这三类：后端也会拒，但前端拦一下
 * 能省一次往返，而且错误挂在字段上比一个 toast 好找。
 */
export function validateCustomerForm(
  form: CustomerFormState,
  isEdit: boolean,
): CustomerFieldErrors {
  const errors: CustomerFieldErrors = {};

  // 编辑态的编码是只读的（后端 `UpdateCustomerReq` 虽允许改，但界面上不给改）
  if (!isEdit) {
    const code = form.code.trim();
    if (!code) {
      errors.code = '客户编码必填';
    } else if (charCount(code) > CUSTOMER_LIMITS.code) {
      errors.code = `客户编码最多 ${CUSTOMER_LIMITS.code} 个字符`;
    }
  }

  const name = form.name.trim();
  if (!name) {
    errors.name = '客户名称必填';
  } else if (charCount(name) > CUSTOMER_LIMITS.name) {
    errors.name = `客户名称最多 ${CUSTOMER_LIMITS.name} 个字符`;
  }

  const lengthRules: [keyof CustomerFieldErrors, string, number, string][] = [
    ['tax_no', form.tax_no, CUSTOMER_LIMITS.tax_no, '税号'],
    ['contact_name', form.contact_name, CUSTOMER_LIMITS.contact_name, '联系人'],
    ['contact_phone', form.contact_phone, CUSTOMER_LIMITS.contact_phone, '联系电话'],
    ['address', form.address, CUSTOMER_LIMITS.address, '地址'],
    ['remark', form.remark, CUSTOMER_LIMITS.remark, '备注'],
  ];
  for (const [key, value, max, label] of lengthRules) {
    if (charCount(value) > max) {
      errors[key] = `${label}最多 ${max} 个字符`;
    }
  }

  if (!Number.isFinite(form.credit_limit) || form.credit_limit < 0) {
    errors.credit_limit = '信用额度不能为负（0 = 不控额度）';
  } else if (form.credit_limit > CUSTOMER_CREDIT_LIMIT_MAX) {
    errors.credit_limit = '信用额度超出可存范围（整数位最多 14 位）';
  }

  return errors;
}

/** 空串 → `null`（后端 `clean_optional` 也认空串，但 `null` 更干净） */
function nullable(text: string): string | null {
  const value = text.trim();
  return value === '' ? null : value;
}

/** 额度：数字 → 字符串定点（`NUMERIC(18,4)` 口径；不走 `toFixed` 以免引入二进制误差） */
export function creditLimitText(value: number): string {
  if (!Number.isFinite(value) || value <= 0) {
    return '0';
  }
  return String(value);
}

/** 新建请求体 */
export function buildCustomerCreatePayload(form: CustomerFormState): CreateCustomerReq {
  return {
    code: form.code.trim(),
    name: form.name.trim(),
    tax_no: nullable(form.tax_no),
    contact_name: nullable(form.contact_name),
    contact_phone: nullable(form.contact_phone),
    address: nullable(form.address),
    credit_limit: creditLimitText(Number(form.credit_limit)),
    status: form.status === 'FROZEN' ? 'FROZEN' : 'ACTIVE',
    remark: nullable(form.remark),
  };
}

/**
 * 编辑请求体：**只发真改过的字段**（缺席 = 不改）。
 *
 * 文本字段从有到无 → 发 `''`（后端 `merge_write`：`Some("")` → `None`，即清空）。
 * 不要拿整表单覆盖：那会把别人刚改的「联系人」按我这台机器的旧值写回去。
 */
export function buildCustomerUpdatePayload(
  form: CustomerFormState,
  original: Customer,
): UpdateCustomerReq {
  const payload: UpdateCustomerReq = {};

  const name = form.name.trim();
  if (name !== original.name.trim()) {
    payload.name = name;
  }

  const textPairs: [keyof CustomerFormState & (
    | 'tax_no'
    | 'contact_name'
    | 'contact_phone'
    | 'address'
    | 'remark'
  ), string, string | null][] = [
    ['tax_no', form.tax_no, original.tax_no],
    ['contact_name', form.contact_name, original.contact_name],
    ['contact_phone', form.contact_phone, original.contact_phone],
    ['address', form.address, original.address],
    ['remark', form.remark, original.remark],
  ];
  for (const [key, next, previous] of textPairs) {
    const nextValue = nullable(next);
    const previousValue = nullable(previous ?? '');
    if (nextValue !== previousValue) {
      // 清空发空串（后端契约：可选文本 `""` = 清空），有值发 trim 后的文本
      payload[key] = nextValue === null ? '' : nextValue;
    }
  }

  const creditLimit = Number(form.credit_limit) || 0;
  if (creditLimit !== Number(original.credit_limit ?? 0)) {
    payload.credit_limit = creditLimitText(creditLimit);
  }

  if (form.status !== original.status) {
    payload.status = form.status === 'FROZEN' ? 'FROZEN' : 'ACTIVE';
  }

  return payload;
}

/**
 * 状态切换的请求体（只带 `status`，最小变更）。
 *
 * 与 `buildCustomerUpdatePayload` 分开是有意的：列表页的「冻结 / 解冻」按钮
 * 不该依赖表单状态，否则用户在弹窗里改了一半再点取消，列表上的切换会带上脏字段。
 */
export function buildCustomerStatusPayload(next: CustomerStatus): UpdateCustomerReq {
  return { status: next === 'FROZEN' ? 'FROZEN' : 'ACTIVE' };
}

/** 有没有实际要提交的字段（全没改时省一次 PUT + 一条「已保存」的假成功） */
export function isCustomerUpdateEmpty(payload: UpdateCustomerReq): boolean {
  return Object.keys(payload).length === 0;
}

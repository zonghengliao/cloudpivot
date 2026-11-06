/**
 * 价格维护页的纯逻辑件（列表列定义 / 过滤器与表单草稿 / 校验 / 提交载荷）。
 *
 * 页面（`index.vue`）只做渲染与调用；凡是「能脱离渲染算出来的」都放这里：
 *
 * 1. **列定义**：`colKey` 与后端出参字段名逐字一致（`price` / `min_qty` / `effective_to` …），
 *    写错了不会报错、只会渲染空列 —— 放这里便于一眼比对 `customer-price.ts` 的接口类型；
 * 2. **表单校验**：与后端 `price_admin_handler.rs` 的四条入口校验**同序同口径**
 *    （价格 > 0 → 起订量 ≥ 0 → 档位上界 > 下界 → 有效期不早于生效日），
 *    先在前端拦住能让用户少跑一次网络往返；后端仍然会再判一遍（它才是真相）；
 * 3. **提交载荷**：只发后端认的字段；`null` / 空串的**语义差别**在这里统一
 *    （可空列传空串 = 清空；「不传」= 不改）。
 *
 * # 三个容易写反的地方
 *
 * * 档位区间是**左闭右开**：`min_qty <= 订购量 < max_qty`，`max_qty` 空 = 无上限；
 * * 有效期两端都**含**：`effective_to = effective_from` 是「只当天有效」，不是空区间；
 * * 编辑档位「清空上限」要发 `max_qty: -1`（JSON 的 null 与"不传"无法区分，
 *   后端约定负数哨兵，见 `UpdateTierReq::merge`）。
 */
import type { PrimaryTableCol } from 'tdesign-vue-next';
import type {
  CreateCustomerPriceReq,
  CreatePriceTierReq,
  CustomerPriceFilter,
  CustomerPriceRow,
  PriceStatus,
  PriceTierFilter,
  PriceTierRow,
  TierType,
  UpdateCustomerPriceReq,
  UpdatePriceTierReq,
} from '@/api/business/erp/customer-price';
import { formatMoney } from '@/views/business/erp/shared/money';

/** 每页条数（与全仓主数据页一致） */
export const PRICE_PAGE_SIZE = 20;

/** 协议价列表列 */
export const AGREED_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'customer', title: '客户', minWidth: 220 },
  { colKey: 'material', title: '物料', minWidth: 240 },
  { colKey: 'price', title: '协议单价（含税）', width: 150, align: 'right' },
  { colKey: 'min_qty', title: '起订量', width: 110, align: 'right' },
  { colKey: 'validity', title: '有效期', width: 210 },
  { colKey: 'status', title: '状态', width: 90 },
  { colKey: 'remark', title: '备注', minWidth: 160, ellipsis: true },
  { colKey: 'op', title: '操作', width: 140, fixed: 'right' },
];

/** 档位列表列（同一物料多档 = 这里的区间列，按期排序后一眼看出阶梯） */
export const TIER_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'material', title: '物料', minWidth: 240 },
  { colKey: 'scope', title: '适用客户', minWidth: 180 },
  { colKey: 'tier_type', title: '类型', width: 100 },
  { colKey: 'range', title: '数量区间', minWidth: 180 },
  { colKey: 'price', title: '档位单价（含税）', width: 160, align: 'right' },
  { colKey: 'validity', title: '有效期', width: 210 },
  { colKey: 'status', title: '状态', width: 90 },
  { colKey: 'op', title: '操作', width: 140, fixed: 'right' },
];

/** 状态下拉 */
export const STATUS_OPTIONS = [
  { label: '启用', value: 'ACTIVE' },
  { label: '停用', value: 'INACTIVE' },
];

/** 档位类型下拉 */
export const TIER_TYPE_OPTIONS = [
  { label: '阶梯价', value: 'TIER' },
  { label: '促销价', value: 'PROMO' },
];

/** 状态中文 */
export function statusLabel(status: string): string {
  return status === 'ACTIVE' ? '启用' : status === 'INACTIVE' ? '停用' : status;
}

/** 状态主题色 */
export function statusTheme(status: string): 'success' | 'default' {
  return status === 'ACTIVE' ? 'success' : 'default';
}

/** 档位类型中文 */
export function tierTypeLabel(tierType: string): string {
  return tierType === 'PROMO' ? '促销价' : '阶梯价';
}

/** 档位类型主题色（促销用 warning 抢眼，阶梯用 primary） */
export function tierTypeTheme(tierType: string): 'warning' | 'primary' {
  return tierType === 'PROMO' ? 'warning' : 'primary';
}

/** 数值文本（去掉无意义的尾零，0.0000 → 0） */
export function decimalText(value: string | number | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '—';
  }
  const num = typeof value === 'number' ? value : Number(value);
  if (!Number.isFinite(num)) {
    return '—';
  }
  return String(Number(num.toFixed(4)));
}

/** 有效期文本（两端都含；空失效日 = 长期有效） */
export function validityText(from: string, to: string | null): string {
  return `${from || '—'} ~ ${to || '长期'}`;
}

/** 客户展示：名称（编码）；客户被物理删时给「客户已删除」 */
export function customerText(row: {
  customer_code: string | null;
  customer_name: string | null;
}): string {
  if (row.customer_name && row.customer_code) {
    return `${row.customer_name}（${row.customer_code}）`;
  }
  if (row.customer_name) {
    return row.customer_name;
  }
  return '客户已删除';
}

/** 物料展示：编码 名称 / 规格 */
export function materialText(row: {
  material_code: string | null;
  material_name: string | null;
  material_spec: string | null;
}): string {
  if (!row.material_code && !row.material_name) {
    return '物料已删除';
  }
  const spec = row.material_spec ? ` / ${row.material_spec}` : '';
  return `${row.material_code ?? ''} ${row.material_name ?? ''}${spec}`.trim();
}

/** 档位适用范围：客户专属 or 通用 */
export function tierScopeText(row: PriceTierRow): string {
  if (!row.customer_id) {
    return '通用（所有客户）';
  }
  return customerText(row);
}

/** 档位区间文本：`[10, 100)`；上界空 = `[10, +∞)` */
export function tierRangeText(row: {
  min_qty: string | number;
  max_qty: string | number | null;
}): string {
  const min = decimalText(row.min_qty);
  const max = row.max_qty === null || row.max_qty === undefined ? '+∞' : decimalText(row.max_qty);
  return `[${min}, ${max})`;
}

/** 列表里的金额（含 4 位小数；与全仓 `formatMoney` 同源） */
export function priceText(value: string | number): string {
  return formatMoney(value);
}

// ==================== 协议价：过滤器 / 表单 ====================

/** 空过滤器 */
export function emptyAgreedFilter(): CustomerPriceFilter {
  return { customer_id: '', material_id: '', status: '', keyword: '', page: 1, page_size: PRICE_PAGE_SIZE };
}

/** 过滤器是否生效（决定「空列表」提示是「还没有数据」还是「筛没了」） */
export function isAgreedFiltered(filter: CustomerPriceFilter): boolean {
  return Boolean(filter.customer_id || filter.material_id || filter.status || filter.keyword);
}

/** 空档位过滤器 */
export function emptyTierFilter(): PriceTierFilter {
  return {
    material_id: '',
    customer_id: '',
    tier_type: '',
    status: '',
    keyword: '',
    page: 1,
    page_size: PRICE_PAGE_SIZE,
  };
}

/** 档位过滤器是否生效 */
export function isTierFiltered(filter: PriceTierFilter): boolean {
  return Boolean(filter.material_id || filter.customer_id || filter.tier_type || filter.status || filter.keyword);
}

/** 协议价草稿（新建 / 编辑同构；`key` 不用，行主键另存 `editingId`） */
export interface AgreedFormDraft {
  customer_id: string;
  material_id: string;
  price: number | string;
  min_qty: number | string;
  effective_from: string;
  effective_to: string;
  status: PriceStatus;
  remark: string;
}

/** 空协议价表单（`today` 由调用方给，便于与「北京时间今天」对齐） */
export function emptyAgreedForm(today: string): AgreedFormDraft {
  return {
    customer_id: '',
    material_id: '',
    price: '',
    min_qty: '0',
    effective_from: today,
    effective_to: '',
    status: 'ACTIVE',
    remark: '',
  };
}

/** 行 → 编辑表单 */
export function agreedFormFrom(row: CustomerPriceRow): AgreedFormDraft {
  return {
    customer_id: row.customer_id,
    material_id: row.material_id,
    price: Number(row.price),
    min_qty: Number(row.min_qty),
    effective_from: row.effective_from,
    effective_to: row.effective_to ?? '',
    status: row.status,
    remark: row.remark ?? '',
  };
}

/** 档位草稿 */
export interface TierFormDraft {
  material_id: string;
  customer_id: string;
  tier_type: TierType;
  min_qty: number | string;
  max_qty: number | string;
  price: number | string;
  effective_from: string;
  effective_to: string;
  status: PriceStatus;
  remark: string;
}

/** 空档位表单 */
export function emptyTierForm(today: string): TierFormDraft {
  return {
    material_id: '',
    customer_id: '',
    tier_type: 'TIER',
    min_qty: '0',
    max_qty: '',
    price: '',
    effective_from: today,
    effective_to: '',
    status: 'ACTIVE',
    remark: '',
  };
}

/** 行 → 编辑表单 */
export function tierFormFrom(row: PriceTierRow): TierFormDraft {
  return {
    material_id: row.material_id,
    customer_id: row.customer_id ?? '',
    tier_type: row.tier_type,
    min_qty: Number(row.min_qty),
    max_qty: row.max_qty === null ? '' : Number(row.max_qty),
    price: Number(row.price),
    effective_from: row.effective_from,
    effective_to: row.effective_to ?? '',
    status: row.status,
    remark: row.remark ?? '',
  };
}

// ==================== 校验（与后端同序同口径） ====================

/** 数值化（空 / 非数 → NaN，便于统一判非法） */
function toNumber(value: number | string): number {
  if (value === '' || value === null || value === undefined) {
    return Number.NaN;
  }
  const num = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(num) ? num : Number.NaN;
}

/** 协议价表单校验：返回第一条人话错误，`null` = 通过 */
export function validateAgreedForm(form: AgreedFormDraft): string | null {
  if (!form.customer_id) {
    return '请选择客户';
  }
  if (!form.material_id) {
    return '请选择物料';
  }
  const price = toNumber(form.price);
  if (!Number.isFinite(price) || price <= 0) {
    return '协议单价必须大于 0（免费品请在订单行填 0 元，不要写进价格表）';
  }
  const minQty = toNumber(form.min_qty === '' ? 0 : form.min_qty);
  if (!Number.isFinite(minQty) || minQty < 0) {
    return '起订量不能为负；0 表示不限制起订量';
  }
  if (!form.effective_from) {
    return '请选择生效日';
  }
  if (form.effective_to && form.effective_to < form.effective_from) {
    return '失效日不能早于生效日（两端都含，同一天表示只当天有效）';
  }
  return null;
}

/** 档位表单校验 */
export function validateTierForm(form: TierFormDraft): string | null {
  if (!form.material_id) {
    return '请选择物料';
  }
  const price = toNumber(form.price);
  if (!Number.isFinite(price) || price <= 0) {
    return '档位单价必须大于 0';
  }
  const minQty = toNumber(form.min_qty);
  if (!Number.isFinite(minQty) || minQty < 0) {
    return '区间下界不能为负（0 表示从 0 起，无下限）';
  }
  if (form.max_qty !== '' && form.max_qty !== null && form.max_qty !== undefined) {
    const maxQty = toNumber(form.max_qty);
    if (!Number.isFinite(maxQty)) {
      return '区间上界必须是数字；无上限请留空';
    }
    if (maxQty <= minQty) {
      return '区间上界必须大于下界（区间是左闭右开，相等就是空区间）；无上限请留空';
    }
  }
  if (!form.effective_from) {
    return '请选择生效日';
  }
  if (form.effective_to && form.effective_to < form.effective_from) {
    return '失效日不能早于生效日';
  }
  return null;
}

// ==================== 提交载荷 ====================

/** 数字文本（保留 4 位，去掉多余尾零；后端按 `NUMERIC(18,4)` 收） */
function qtyText(value: number | string, fallback = '0'): string {
  const num = toNumber(value === '' ? fallback : value);
  return Number.isFinite(num) ? String(Number(num.toFixed(4))) : fallback;
}

/** 协议价新建载荷 */
export function buildAgreedCreatePayload(form: AgreedFormDraft): CreateCustomerPriceReq {
  return {
    customer_id: form.customer_id,
    material_id: form.material_id,
    price: qtyText(form.price),
    min_qty: qtyText(form.min_qty),
    effective_from: form.effective_from,
    effective_to: form.effective_to || null,
    status: form.status,
    remark: form.remark.trim() || null,
  };
}

/**
 * 协议价编辑载荷：**整行发**（后端是部分更新，但表单弹窗里所有字段都可见，
 * 发整行语义更直观：用户在弹窗里看到的 = 提交后的状态）。
 */
export function buildAgreedUpdatePayload(form: AgreedFormDraft): UpdateCustomerPriceReq {
  return buildAgreedCreatePayload(form);
}

/** 档位新建载荷 */
export function buildTierCreatePayload(form: TierFormDraft): CreatePriceTierReq {
  return {
    material_id: form.material_id,
    customer_id: form.customer_id || null,
    tier_type: form.tier_type,
    min_qty: qtyText(form.min_qty),
    max_qty: form.max_qty === '' ? null : qtyText(form.max_qty),
    price: qtyText(form.price),
    effective_from: form.effective_from,
    effective_to: form.effective_to || null,
    status: form.status,
    remark: form.remark.trim() || null,
  };
}

/**
 * 档位编辑载荷：与新建同构，但「上界留空」要发 `-1`（清空哨兵）。
 *
 * 为什么不能发 `null`：JSON `null` 与「字段缺省」在后端反序列化后都是 `None` = 不改，
 * 想把已有的上界清成「无上限」就必须用哨兵（后端 `UpdateTierReq::merge` 约定）。
 */
export function buildTierUpdatePayload(form: TierFormDraft): UpdatePriceTierReq {
  return {
    ...buildTierCreatePayload(form),
    max_qty: form.max_qty === '' ? -1 : qtyText(form.max_qty),
  };
}

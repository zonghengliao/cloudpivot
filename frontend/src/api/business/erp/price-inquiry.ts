/**
 * 询比价 API 层（后端 `backend/src/business/erp/price_inquiry_handler.rs`，P2-01）。
 *
 * # 接口
 *
 * | 动作 | 接口 | 权限码 |
 * |------|------|--------|
 * | 分页 | `GET /api/business/erp/price-inquiry/page` | `business:erp:price-inquiry:list` |
 * | 详情（含比价表） | `GET /api/business/erp/price-inquiry/{id}` | 同上 |
 * | 建单 | `POST /api/business/erp/price-inquiry` | `…:create` |
 * | 录入报价 | `POST /api/business/erp/price-inquiry/{id}/quote` | `…:quote` |
 * | 定标 | `POST /api/business/erp/price-inquiry/{id}/award` | `…:award` |
 * | 转采购订单 | `POST /api/business/erp/price-inquiry/{id}/to-po` | `…:to-po` |
 *
 * # 三个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端注解不含前缀，漏了会命中 SPA fallback 返回 HTML）；
 * 2. **金额是字符串**（后端 `Decimal`）：提交前 `String(value)`，展示用 `decimalText()`；
 * 3. **含税单价不传**：后端按 `price_excl × (1 + tax_rate/100)` 算，前端只提交不含税单价
 *    —— 传了也会被 serde 静态忽略（`QuoteItemReq` 里没有这个字段）。
 */
import { get, post, type PageData } from '@/api/request';

import type { DocMeta } from './doc-meta';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/** 后端 `NUMERIC` 在 JSON 里是字符串，`t-input-number` 给数字 */
export type DecimalText = string | number;

/** 询价单状态（与 `ck_biz_piq_status` 逐字一致） */
export type InquiryStatus = 'DRAFT' | 'QUOTING' | 'AWARDED' | 'CONVERTED';

/** 定标方式 */
export type AwardMode = 'MULTI' | 'SINGLE_SOURCE';

/** 状态下拉（顺序与后端 STATUSES 一致） */
export const STATUS_OPTIONS: { value: InquiryStatus; label: string }[] = [
  { value: 'DRAFT', label: '草稿' },
  { value: 'QUOTING', label: '报价中' },
  { value: 'AWARDED', label: '已定标' },
  { value: 'CONVERTED', label: '已转采购单' },
];

/** 状态标签（未知值原样返回，不吞信息） */
export function statusLabel(status: string): string {
  return STATUS_OPTIONS.find((item) => item.value === status)?.label ?? status;
}

/** 状态主题（t-tag） */
export function statusTheme(status: string): 'default' | 'primary' | 'success' | 'warning' {
  switch (status) {
    case 'DRAFT':
      return 'default';
    case 'QUOTING':
      return 'warning';
    case 'AWARDED':
      return 'primary';
    case 'CONVERTED':
      return 'success';
    default:
      return 'default';
  }
}

/** 金额 / 数量展示（字符串 → 原样，数字 → 补齐） */
export function decimalText(value: DecimalText | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '—';
  }
  return String(value);
}

/** 转单结果里的一张采购订单 */
export interface CreatedPurchaseOrder {
  /** 采购订单 id */
  po_id: string;
  /** 采购订单号 */
  po_no: string;
  /** 供应商 id */
  supplier_id: string;
  /** 供应商名 */
  supplier_name: string;
  /** 行数 */
  line_count: number;
  /** 价税合计 */
  total_with_tax: DecimalText;
}

/** 询价单列表行 / 单头 */
export interface PriceInquiryRow {
  /** 主键 */
  id: string;
  /** 询价单号 PIQ-yyyyMMdd-000001 */
  inquiry_no: string;
  /** 询价日期 */
  inquiry_date: string;
  /** 采购员 id */
  buyer_id: string | null;
  /** 采购员姓名快照 */
  buyer_name: string | null;
  /** 收货仓库 id */
  warehouse_id: string;
  /** 收货仓库名 */
  warehouse_name: string;
  /** 状态 */
  status: InquiryStatus;
  /** 状态标签 */
  status_label: string;
  /** 明细行数 */
  item_count: number;
  /** 已报价供应商家数 */
  supplier_count: number;
  /** 定标方式 */
  award_mode: AwardMode | null;
  /** 定标方式标签 */
  award_mode_label: string;
  /** 单一供应商定标时的中标供应商 */
  award_supplier_id: string | null;
  /** 中标供应商名 */
  award_supplier_name: string | null;
  /** 备注 */
  remark: string | null;
  /** 定标时间 */
  awarded_at: string | null;
  /** 已生成的采购订单引用 */
  po_refs: { po_id: string; po_no: string; supplier_id: string; line_count: number }[];
  /** 创建时间 */
  created_at: string;
}

/** 询价明细 */
export interface PriceInquiryItem {
  /** 主键 */
  id: string;
  /** 行号 */
  line_no: number;
  /** 物料 id */
  material_id: string;
  /** 物料编码 */
  material_code: string;
  /** 物料名称 */
  material_name: string;
  /** 单位 id */
  unit_id: string;
  /** 单位名 */
  unit_name: string;
  /** 数量 */
  qty: DecimalText;
  /** 需求日期 */
  demand_date: string | null;
  /** 行备注 */
  remark: string | null;
}

/** 报价行 */
export interface PriceQuoteRow {
  /** 主键 */
  id: string;
  /** 明细 id */
  item_id: string;
  /** 供应商 id */
  supplier_id: string;
  /** 供应商名 */
  supplier_name: string;
  /** 不含税单价 */
  price_excl: DecimalText;
  /** 含税单价（服务端算） */
  price_incl: DecimalText;
  /** 税率 */
  tax_rate: DecimalText;
  /** 交期天数 */
  delivery_days: number | null;
  /** 承诺到货日 */
  delivery_date: string | null;
  /** 备注 */
  remark: string | null;
  /** 是否中标 */
  is_awarded: boolean;
}

/** 比价表的一格 */
export interface CompareQuote {
  /** 报价 id */
  quote_id: string;
  /** 供应商 id */
  supplier_id: string;
  /** 供应商名 */
  supplier_name: string;
  /** 不含税单价 */
  price_excl: DecimalText;
  /** 含税单价 */
  price_incl: DecimalText;
  /** 税率 */
  tax_rate: DecimalText;
  /** 交期天数 */
  delivery_days: number | null;
  /** 承诺到货日 */
  delivery_date: string | null;
  /** 备注 */
  remark: string | null;
  /** 是否本行最低不含税价 */
  is_lowest: boolean;
  /** 是否中标 */
  is_awarded: boolean;
}

/** 比价表的一行（一条明细横向并排所有报价） */
export interface CompareRow {
  /** 明细 id */
  item_id: string;
  /** 行号 */
  line_no: number;
  /** 物料 id */
  material_id: string;
  /** 物料编码 */
  material_code: string;
  /** 物料名称 */
  material_name: string;
  /** 单位名 */
  unit_name: string;
  /** 数量 */
  qty: DecimalText;
  /** 需求日期 */
  demand_date: string | null;
  /** 本行报价家数 */
  bidder_count: number;
  /** 是否满足「至少 2 家」 */
  has_competition: boolean;
  /** 各行报价 */
  quotes: CompareQuote[];
}

/** 询价单详情（`price_inquiry_dto.rs::PriceInquiryDetailRes`） */
export interface PriceInquiryDetail {
  /**
   * 单头（统一三段式口径 `data.header`，E2E01-FIX-4）
   *
   * 前端一律读这个字段；`head` 是兼容期保留的旧名，不要再用。
   */
  header: PriceInquiryRow;
  /** 明细 */
  items: PriceInquiryItem[];
  /** 附属信息（`data.meta`）：报价条数 / 比价行数 / 是否可转单 */
  meta: DocMeta;
  /**
   * @deprecated 与 `header` 同值，兼容期保留。新代码读 `header`。
   */
  head: PriceInquiryRow;
  /** 全部报价（扁平） */
  quotes: PriceQuoteRow[];
  /** 比价表 */
  comparison: CompareRow[];
  /** 是否可以转采购订单 */
  can_to_po: boolean;
  /** 不能转单的原因 */
  to_po_blocked_reason: string | null;
}

/** 分页筛选 */
export interface PriceInquiryFilter {
  /** 页码 */
  page?: number;
  /** 每页条数 */
  page_size?: number;
  /** 状态 */
  status?: InquiryStatus | '';
  /** 关键字 */
  keyword?: string;
  /** 询价日期起 */
  date_from?: string;
  /** 询价日期止 */
  date_to?: string;
}

/** 建单明细入参 */
export interface PriceInquiryItemPayload {
  /** 物料 */
  material_id: string;
  /** 数量（字符串提交） */
  qty: string;
  /** 需求日期 */
  demand_date?: string | null;
  /** 行备注 */
  remark?: string | null;
}

/** 建单入参 */
export interface CreatePriceInquiryPayload {
  /** 收货仓库 */
  warehouse_id: string;
  /** 询价日期 */
  inquiry_date?: string | null;
  /** 采购员（缺省 = 当前登录人） */
  buyer_id?: string | null;
  /** 备注 */
  remark?: string | null;
  /** 明细 */
  items: PriceInquiryItemPayload[];
}

/** 报价行入参（没有 price_incl：含税价服务端算） */
export interface QuoteItemPayload {
  /** 明细 id */
  item_id: string;
  /** 不含税单价 */
  price_excl: string;
  /** 税率（0–100） */
  tax_rate?: string;
  /** 交期天数 */
  delivery_days?: number | null;
  /** 承诺到货日 */
  delivery_date?: string | null;
  /** 备注 */
  remark?: string | null;
}

/** 录入一家供应商的报价 */
export interface QuotePayload {
  /** 供应商 */
  supplier_id: string;
  /** 报价行 */
  items: QuoteItemPayload[];
}

/** 定标入参（items 优先；只给 supplier_id = 全部明细都给这一家） */
export interface AwardPayload {
  /** 单一供应商 */
  supplier_id?: string;
  /** 按明细定标 */
  items?: { item_id: string; supplier_id: string }[];
  /** 单源例外理由（≥10 字） */
  single_source_reason?: string;
}

/** 转采购订单入参 */
export interface ToPoPayload {
  /** 下单日期 */
  order_date?: string | null;
  /** 收货仓库覆盖 */
  warehouse_id?: string | null;
  /** 采购单备注前缀 */
  remark?: string | null;
}

/** 转单结果 */
export interface ToPoResult {
  /** 询价单 id */
  inquiry_id: string;
  /** 询价单号 */
  inquiry_no: string;
  /** 转单后状态 */
  status: InquiryStatus;
  /** 生成的采购订单 */
  purchase_orders: CreatedPurchaseOrder[];
}

/** 去掉空串参数（后端把 `?keyword=` 当「等于空串」过滤，会查出 0 行） */
function asParams<T extends object>(query: T): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  Object.entries(query).forEach(([key, value]) => {
    if (value === undefined || value === null || value === '') {
      return;
    }
    out[key] = value;
  });
  return out;
}

/** 分页查询询价单 */
export function fetchPriceInquiryPage(
  query: PriceInquiryFilter,
): Promise<PageData<PriceInquiryRow>> {
  return get<PageData<PriceInquiryRow>>(`${BASE}/price-inquiry/page`, asParams(query));
}

/** 询价单详情（含比价表） */
export function fetchPriceInquiryDetail(id: string): Promise<PriceInquiryDetail> {
  return get<PriceInquiryDetail>(`${BASE}/price-inquiry/${id}`);
}

/** 新建询价单 */
export function createPriceInquiry(payload: CreatePriceInquiryPayload): Promise<PriceInquiryDetail> {
  return post<PriceInquiryDetail>(`${BASE}/price-inquiry`, payload);
}

/** 录入一家供应商的报价（重复录入 = 改价） */
export function submitPriceQuote(id: string, payload: QuotePayload): Promise<PriceInquiryDetail> {
  return post<PriceInquiryDetail>(`${BASE}/price-inquiry/${id}/quote`, payload);
}

/** 定标 */
export function awardPriceInquiry(id: string, payload: AwardPayload): Promise<PriceInquiryDetail> {
  return post<PriceInquiryDetail>(`${BASE}/price-inquiry/${id}/award`, payload);
}

/** 转采购订单（按中标供应商分组，一单可拆多单） */
export function convertPriceInquiryToPo(id: string, payload: ToPoPayload): Promise<ToPoResult> {
  return post<ToPoResult>(`${BASE}/price-inquiry/${id}/to-po`, payload);
}

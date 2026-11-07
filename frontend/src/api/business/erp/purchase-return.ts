/**
 * 采购退货 API 层（后端 `backend/src/business/erp/purchase_return_handler.rs`，P2-03）。
 *
 * | 动作 | 接口 | 权限码 |
 * |------|------|--------|
 * | 分页 | `GET /api/business/erp/purchase-return/page` | `business:erp:purchase-return:list` |
 * | 详情（单头 + 明细） | `GET /api/business/erp/purchase-return/{id}` | 同上 |
 * | 可退明细（选入库单） | `GET /api/business/erp/purchase-return/source/{stockInId}` | `…:create` |
 * | 建退货单 | `POST /api/business/erp/purchase-return` | `…:create` |
 * | 提交审批 | `POST /api/business/erp/purchase-return/{id}/submit` | `…:submit` |
 * | 确认退货 | `POST /api/business/erp/purchase-return/{id}/confirm` | `…:confirm` |
 * | 作废 | `POST /api/business/erp/purchase-return/{id}/cancel` | `…:confirm` |
 *
 * # 三个必须记住的点
 *
 * 1. **路径必须写全 `/api`**（后端注解不含前缀，漏了会命中 SPA fallback 返回 HTML）；
 * 2. **金额是字符串**（后端 `Decimal`）：展示用 `decimalText()`，提交时不用传金额；
 * 3. **单价与金额由后端算**：建单只提交 `stock_in_item_id` + `qty`（`unit_price` 仅用于
 *    「防价格操纵」校验，前端不要自己算钱）。
 */
import { get, post, type PageData } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/** 后端 `NUMERIC` 在 JSON 里是字符串，`t-input-number` 给数字 */
export type DecimalText = string | number;

/** 退货单状态（与 `ck_biz_prt_status` 逐字一致） */
export type ReturnStatus = 'DRAFT' | 'SUBMITTED' | 'APPROVED' | 'CONFIRMED' | 'CANCELLED';

/** 状态下拉（顺序与后端 STATUSES 一致） */
export const STATUS_OPTIONS: { value: ReturnStatus; label: string }[] = [
  { value: 'DRAFT', label: '草稿' },
  { value: 'SUBMITTED', label: '审批中' },
  { value: 'APPROVED', label: '已批准' },
  { value: 'CONFIRMED', label: '已确认' },
  { value: 'CANCELLED', label: '已作废' },
];

/** 状态标签（未知值原样返回，不吞信息） */
export function statusLabel(status: string): string {
  return STATUS_OPTIONS.find((item) => item.value === status)?.label ?? status;
}

/** 状态主题（t-tag） */
export function statusTheme(status: string): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  switch (status) {
    case 'DRAFT':
      return 'default';
    case 'SUBMITTED':
      return 'warning';
    case 'APPROVED':
      return 'primary';
    case 'CONFIRMED':
      return 'success';
    case 'CANCELLED':
      return 'danger';
    default:
      return 'default';
  }
}

/** 金额展示（后端字符串 → 4 位定标文本） */
export function decimalText(value: DecimalText | null | undefined, digits = 2): string {
  if (value === null || value === undefined || value === '') return '0.00';
  const num = Number(value);
  if (!Number.isFinite(num)) return String(value);
  return num.toFixed(digits);
}

/** 列表行 */
export interface ReturnRow {
  id: string;
  return_no: string;
  stock_in_id: string;
  stock_in_no: string;
  purchase_order_no: string | null;
  supplier_id: string;
  supplier_name: string | null;
  warehouse_id: string;
  warehouse_name: string | null;
  return_date: string;
  status: ReturnStatus;
  status_label: string;
  total_qty: string;
  total_amount: string;
  total_tax: string;
  total_with_tax: string;
  reason: string;
  approval_instance_id: string | null;
  stock_out_no: string | null;
  payable_no: string | null;
  voucher_no: string | null;
  supplier_receivable_no: string | null;
  remark: string | null;
  created_at: string;
}

/** 明细行 */
export interface ReturnItem {
  id: string;
  line_no: number;
  stock_in_item_id: string;
  material_id: string;
  material_code: string | null;
  material_name: string | null;
  unit_id: string;
  unit_name: string | null;
  qty: string;
  price: string;
  price_incl: string;
  tax_rate: string;
  amount: string;
  tax_amount: string;
  with_tax_amount: string;
  received_qty: string;
  returned_qty: string;
  location_id: string | null;
  remark: string | null;
}

/** 详情 */
export interface ReturnDetail {
  purchase_return: ReturnRow;
  items: ReturnItem[];
}

/** 可退明细行 */
export interface ReturnableItem {
  stock_in_item_id: string;
  line_no: number;
  material_id: string;
  material_code: string | null;
  material_name: string | null;
  unit_id: string;
  unit_name: string | null;
  received_qty: string;
  returned_qty: string;
  returnable_qty: string;
  price: string;
  price_incl: string;
  tax_rate: string;
  location_id: string | null;
}

/** 可退明细（入库单 + 明细） */
export interface ReturnableSource {
  stock_in_id: string;
  stock_in_no: string;
  in_date: string;
  supplier_id: string | null;
  supplier_name: string | null;
  warehouse_id: string;
  purchase_order_id: string | null;
  purchase_order_no: string | null;
  items: ReturnableItem[];
}

/** 分页查询 */
export interface ReturnPageQuery {
  page?: number;
  page_size?: number;
  status?: string;
  keyword?: string;
  date_from?: string;
  date_to?: string;
  stock_in_id?: string;
}

/** 建单明细 */
export interface CreateReturnItem {
  stock_in_item_id: string;
  qty: number;
  /** 只用于「单价锁死」校验（后端不接受改价） */
  unit_price?: number;
  remark?: string;
}

/** 建单请求 */
export interface CreateReturnReq {
  stock_in_id: string;
  return_date?: string;
  reason: string;
  remark?: string;
  items: CreateReturnItem[];
}

/** 分页 */
export function pagePurchaseReturn(query: ReturnPageQuery) {
  return get<PageData<ReturnRow>>(`${BASE}/purchase-return/page`, { params: query });
}

/** 详情 */
export function getPurchaseReturn(id: string) {
  return get<ReturnDetail>(`${BASE}/purchase-return/${id}`);
}

/** 可退明细（选入库单带明细） */
export function getReturnableSource(stockInId: string) {
  return get<ReturnableSource>(`${BASE}/purchase-return/source/${stockInId}`);
}

/** 建单 */
export function createPurchaseReturn(data: CreateReturnReq) {
  return post<ReturnDetail>(`${BASE}/purchase-return`, data);
}

/** 提交审批 */
export function submitPurchaseReturn(id: string, reason?: string) {
  return post<ReturnDetail>(`${BASE}/purchase-return/${id}/submit`, reason ? { reason } : {});
}

/** 确认退货 */
export function confirmPurchaseReturn(id: string, reason?: string) {
  return post<ReturnDetail>(`${BASE}/purchase-return/${id}/confirm`, reason ? { reason } : {});
}

/** 作废 */
export function cancelPurchaseReturn(id: string, reason?: string) {
  return post<ReturnDetail>(`${BASE}/purchase-return/${id}/cancel`, reason ? { reason } : {});
}

/** 可退数量（前端展示：`入库 − 已退`，后端是唯一权威） */
export function returnableOf(received: DecimalText, returned: DecimalText): number {
  const left = Number(received) - Number(returned);
  return left > 0 ? left : 0;
}

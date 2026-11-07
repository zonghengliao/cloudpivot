/**
 * 采购申请 API 层（后端 `backend/src/business/erp/purchase_requisition_handler.rs`，P2-02）。
 *
 * # 接口
 *
 * | 动作 | 接口 | 权限码 |
 * |------|------|--------|
 * | 分页 | `GET /api/business/erp/purchase-requisition/page` | `business:erp:purchase-requisition:list` |
 * | 详情（单头 + 明细） | `GET /api/business/erp/purchase-requisition/{id}` | 同上 |
 * | 建申请 | `POST /api/business/erp/purchase-requisition` | `…:create` |
 * | 提交审批 | `POST /api/business/erp/purchase-requisition/{id}/submit` | `…:submit` |
 * | 转采购订单（可合并） | `POST /api/business/erp/purchase-requisition/{id}/to-po` | `…:to-po` |
 *
 * # 四个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端注解不含前缀，漏了会命中 SPA fallback 返回 HTML）；
 * 2. **金额是字符串**（后端 `Decimal`）：提交前 `String(value)`，展示用 `decimalText()`；
 * 3. **合并转单**：`to-po` 的路径 id 之外，`requisition_ids` 里可以带多张申请单
 *    （同物料数量相加、单价按数量加权平均），**路径 id 必须在列表里**；
 * 4. **含税单价 / 行金额不传**：后端按预估不含税单价现算，前端只提交 `est_price`。
 */
import { get, post, type PageData } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/** 后端 `NUMERIC` 在 JSON 里是字符串，`t-input-number` 给数字 */
export type DecimalText = string | number;

/** 采购申请状态（与 `ck_biz_prq_status` 逐字一致） */
export type RequisitionStatus = 'DRAFT' | 'SUBMITTED' | 'APPROVED' | 'CONVERTED';

/** 状态下拉（顺序与后端 STATUSES 一致） */
export const STATUS_OPTIONS: { value: RequisitionStatus; label: string }[] = [
  { value: 'DRAFT', label: '草稿' },
  { value: 'SUBMITTED', label: '审批中' },
  { value: 'APPROVED', label: '已审批' },
  { value: 'CONVERTED', label: '已转单' },
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
    case 'SUBMITTED':
      return 'warning';
    case 'APPROVED':
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

/** 申请单列表行 / 单头 */
export interface RequisitionRow {
  /** 主键 */
  id: string;
  /** 申请单号 PRQ-yyyyMMdd-000001 */
  req_no: string;
  /** 申请日期 */
  req_date: string;
  /** 申请部门 id */
  dept_id: string;
  /** 申请部门名快照 */
  dept_name: string;
  /** 申请人 id */
  applicant_id: string | null;
  /** 申请人姓名快照 */
  applicant_name: string | null;
  /** 需求日期 */
  demand_date: string | null;
  /** 收货仓库 id */
  warehouse_id: string;
  /** 收货仓库名 */
  warehouse_name: string;
  /** 状态 */
  status: RequisitionStatus;
  /** 状态标签 */
  status_label: string;
  /** 预估金额合计（不含税） */
  est_amount: DecimalText;
  /** 明细行数 */
  item_count: number;
  /** 是否超预算 */
  over_budget: boolean;
  /** 超出金额 */
  over_amount: DecimalText;
  /** 预算判定说明 */
  budget_reason: string | null;
  /** 是否做过预算判定 */
  budget_checked: boolean;
  /** 审批实例 id */
  approval_instance_id: string | null;
  /** 采购订单 id（已转单） */
  po_id: string | null;
  /** 采购订单号 */
  po_no: string | null;
  /** 转单时间 */
  converted_at: string | null;
  /** 备注 */
  remark: string | null;
  /** 创建时间 */
  created_at: string;
}

/** 申请明细行 */
export interface RequisitionItem {
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
  /** 预估不含税单价 */
  est_price: DecimalText;
  /** 预估含税单价（服务端算） */
  est_price_incl: DecimalText;
  /** 税率 */
  tax_rate: DecimalText;
  /** 行金额 */
  amount: DecimalText;
  /** 需求日期 */
  demand_date: string | null;
  /** 建议供应商 id */
  suggest_supplier_id: string | null;
  /** 建议供应商名 */
  suggest_supplier_name: string | null;
  /** 已并进的采购订单明细行 */
  po_item_id: string | null;
  /** 行备注 */
  remark: string | null;
}

/** 详情 = 单头 + 明细 */
export interface RequisitionDetail {
  /** 单头 */
  requisition: RequisitionRow;
  /** 明细 */
  items: RequisitionItem[];
}

/** 生成的一张采购订单 */
export interface CreatedPurchaseOrder {
  /** 采购订单 id */
  po_id: string;
  /** 采购订单号 */
  po_no: string;
  /** 供应商 id */
  supplier_id: string;
  /** 供应商名 */
  supplier_name: string;
  /** 明细行数 */
  line_count: number;
  /** 价税合计 */
  total_with_tax: DecimalText;
}

/** 合并后的一行（一个物料） */
export interface MergedLine {
  /** 采购订单行号 */
  line_no: number;
  /** 物料 id */
  material_id: string;
  /** 物料编码 */
  material_code: string;
  /** 物料名称 */
  material_name: string;
  /** 合并后数量 */
  qty: DecimalText;
  /** 加权平均单价 */
  est_price: DecimalText;
  /** 税率 */
  tax_rate: DecimalText;
  /** 需求日期（最早） */
  demand_date: string | null;
  /** 并进这一行的申请单号 */
  source_req_nos: string[];
  /** 并进这一行的申请明细 id */
  source_item_ids: string[];
  /** 对应的采购订单明细 id */
  po_item_id: string | null;
}

/** 转单结果 */
export interface ToPoResult {
  /** 生成的采购订单 */
  purchase_order: CreatedPurchaseOrder;
  /** 参与合并的申请单号 */
  requisition_nos: string[];
  /** 合并后的明细 */
  lines: MergedLine[];
  /** 合并了多少张申请单 */
  merged_count: number;
}

/** 分页查询条件 */
export interface RequisitionFilter {
  /** 页码 */
  page?: number;
  /** 每页条数 */
  page_size?: number;
  /** 状态 */
  status?: RequisitionStatus | '';
  /** 关键字 */
  keyword?: string;
  /** 申请日期起 */
  date_from?: string;
  /** 申请日期止 */
  date_to?: string;
  /** 只看可转单（APPROVED 且未转过） */
  convertible_only?: boolean;
}

/** 建单明细入参 */
export interface RequisitionItemPayload {
  /** 物料 */
  material_id: string;
  /** 数量（字符串提交） */
  qty: string;
  /** 预估不含税单价（字符串提交） */
  est_price: string;
  /** 税率（0–100） */
  tax_rate?: string;
  /** 行需求日期 */
  demand_date?: string | null;
  /** 建议供应商 */
  suggest_supplier_id?: string | null;
  /** 行备注 */
  remark?: string | null;
}

/** 建单入参 */
export interface CreateRequisitionPayload {
  /** 申请部门 */
  dept_id: string;
  /** 收货仓库 */
  warehouse_id: string;
  /** 申请日期 */
  req_date?: string | null;
  /** 申请人（缺省 = 当前登录人） */
  applicant_id?: string | null;
  /** 需求日期 */
  demand_date?: string | null;
  /** 备注 */
  remark?: string | null;
  /** 明细 */
  items: RequisitionItemPayload[];
}

/** 提交审批入参（`skip_*` 只在没配流程时用得上） */
export interface SubmitPayload {
  /** 说明 */
  reason?: string | null;
  /** 显式跳过审批（需 business:erp:skip-approval + ≥10 字理由） */
  skip_approval?: boolean;
  /** 跳过理由 */
  skip_reason?: string | null;
}

/** 转采购订单入参 */
export interface ToPoPayload {
  /** 参与合并的申请单 id（含路径 id；不传 = 只转这一张） */
  requisition_ids?: string[];
  /** 供应商（不给则按明细建议供应商推断） */
  supplier_id?: string | null;
  /** 收货仓库覆盖 */
  warehouse_id?: string | null;
  /** 下单日期 */
  order_date?: string | null;
  /** 期望到货日（缺省 = 合并后最早需求日期） */
  expect_date?: string | null;
  /** 备注 */
  remark?: string | null;
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

/** 分页查询采购申请 */
export function fetchRequisitionPage(
  query: RequisitionFilter,
): Promise<PageData<RequisitionRow>> {
  return get<PageData<RequisitionRow>>(`${BASE}/purchase-requisition/page`, asParams(query));
}

/** 采购申请详情 */
export function fetchRequisitionDetail(id: string): Promise<RequisitionDetail> {
  return get<RequisitionDetail>(`${BASE}/purchase-requisition/${id}`);
}

/** 新建采购申请 */
export function createRequisition(payload: CreateRequisitionPayload): Promise<RequisitionDetail> {
  return post<RequisitionDetail>(`${BASE}/purchase-requisition`, payload);
}

/** 提交审批（走 pr_approve；超预算自动加一道财务经理特殊审批） */
export function submitRequisition(id: string, payload: SubmitPayload = {}): Promise<RequisitionDetail> {
  return post<RequisitionDetail>(`${BASE}/purchase-requisition/${id}/submit`, payload);
}

/** 转采购订单（`requisition_ids` 带多张 = **合并采购**） */
export function convertRequisitionToPo(id: string, payload: ToPoPayload): Promise<ToPoResult> {
  return post<ToPoResult>(`${BASE}/purchase-requisition/${id}/to-po`, payload);
}

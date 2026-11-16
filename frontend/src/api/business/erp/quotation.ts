/**
 * 销售报价单 API 层（后端 `backend/app/src/business/erp/sales/quotation_handler.rs`，M3-02/03/04 → 前端 M3-06）。
 *
 * # 接口表（与 handler 模块文档逐条对应）
 *
 * | 动作 | 接口 | 权限码 |
 * |------|------|--------|
 * | 分页 | `GET /api/business/erp/quotation/page` | `business:erp:quotation:list` |
 * | 详情（含明细 + 审批摘要） | `GET /api/business/erp/quotation/{id}` | 同上 |
 * | 建单（取号 `QT-yyyyMMdd-000001`） | `POST /api/business/erp/quotation` | `…:create` |
 * | 改单（**只有 DRAFT**） | `PUT /api/business/erp/quotation/{id}` | `…:update` |
 * | 状态流转（6 条合法边） | `PATCH /api/business/erp/quotation/{id}/status` | `…:status` |
 * | 软删（**只有 DRAFT / CANCELLED**） | `DELETE /api/business/erp/quotation/{id}` | `…:delete` |
 * | 提交审批（发起 `quotation_approve`） | `POST /api/business/erp/quotation/{id}/submit-approval` | `…:status` |
 * | 转销售订单（带行） | `POST /api/business/erp/quotation/{id}/convert` | `…:convert` |
 *
 * # 五个不能猜的点
 *
 * 1. **路径必须写全 `/api`**：后端注解不含前缀，漏了会命中 SPA fallback 返回 HTML，
 *    报「响应体格式不符合统一响应规范」（AGENTS.md 高频踩坑第 1 条）；
 * 2. **金额是字符串**：后端 `Decimal` 序列化成字符串，提交前 `String(value)`，展示走
 *    `@/views/business/erp/shared/money` 的 `formatMoney`；
 * 3. **`status` 不等于「审批中」**：审批不是状态（M3-04 口径）——`DRAFT` 的单子也可能正在
 *    审批（`approval.status === 'PENDING'`）。判「待批」只看 `approval` 段；
 * 4. **`expired` 是现算的**：`today > valid_until`，不是库里的状态值，也不是 `status` 的一部分；
 * 5. **转单只认 `ACCEPTED`**：`SENT` 先标接受（`PATCH status=ACCEPTED`，那条边自带「未过期」守卫）；
 *    已转过再点 → 200 回原订单，`meta.already_converted === true`（幂等，不建第二张）。
 */
import { del, get, patch, post, put, type PageData } from '@/api/request';

import type { DocMeta } from './doc-meta';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/** 后端 `NUMERIC` 在 JSON 里是字符串，`t-input-number` 给数字 */
export type DecimalText = string | number;

/** 报价单状态（与 `ck_biz_quotation_status` / `quotation_model::STATUSES` 逐字一致） */
export type QuotationStatus = 'DRAFT' | 'SENT' | 'ACCEPTED' | 'REJECTED' | 'CANCELLED';

/** 状态下拉（顺序与后端 `STATUSES` 一致） */
export const STATUS_OPTIONS: { value: QuotationStatus; label: string }[] = [
  { value: 'DRAFT', label: '草稿' },
  { value: 'SENT', label: '已发出' },
  { value: 'ACCEPTED', label: '已接受' },
  { value: 'REJECTED', label: '已拒绝' },
  { value: 'CANCELLED', label: '已作废' },
];

/** 状态标签（未知值原样返回，不吞信息） */
export function statusLabel(status: string): string {
  return STATUS_OPTIONS.find((item) => item.value === status)?.label ?? status;
}

/** 状态主题（t-tag），与销售订单页同一套色：草稿灰 / 已发蓝 / 已接受绿 / 已拒绝红 / 作废浅色 */
export function statusTheme(status: string): 'default' | 'primary' | 'success' | 'danger' | 'warning' {
  switch (status) {
    case 'DRAFT':
      return 'default';
    case 'SENT':
      return 'primary';
    case 'ACCEPTED':
      return 'success';
    case 'REJECTED':
      return 'danger';
    case 'CANCELLED':
      return 'warning';
    default:
      return 'default';
  }
}

/** 有效期筛选（后端 `expired: Option<bool>`，三态：不传 = 全部） */
export type ExpiredFilter = 'ALL' | 'ACTIVE' | 'EXPIRED';

/** 有效期筛选项（`''` 不能当「全部」——TDesign 把空串显示成 placeholder） */
export const EXPIRED_OPTIONS: { value: ExpiredFilter; label: string }[] = [
  { value: 'ALL', label: '全部有效期' },
  { value: 'ACTIVE', label: '有效期内' },
  { value: 'EXPIRED', label: '已过期' },
];

/** 明细出参（后端 `QuotationLineRes`） */
export interface QuotationLine {
  /** 主键 */
  id: string;
  /** 行号（1 起） */
  line_no: number;
  /** 物料 id */
  material_id: string;
  /** 物料编码（软删物料 → null） */
  material_code: string | null;
  /** 物料名称（软删物料 → null） */
  material_name: string | null;
  /** 规格 */
  spec: string | null;
  /** 计量单位名 */
  unit_name: string | null;
  /** 数量 */
  qty: DecimalText;
  /** 不含税单价 */
  price: DecimalText;
  /** 行不含税金额 */
  amount: DecimalText;
  /** 税率百分数 */
  tax_rate: DecimalText;
  /** 行税额 */
  tax_amount: DecimalText;
  /** 行含税金额（= amount + tax_amount） */
  with_tax: DecimalText;
  /** 行备注 */
  remark: string | null;
}

/** 审批摘要（详情页 `approval` 段；列表页恒 null —— 防 N+1） */
export interface QuotationApproval {
  /** 审批实例 id */
  instance_id: string;
  /** 审批单号（`FLW-yyyyMMdd-XXXXXXXX`） */
  instance_no: string;
  /** 实例状态（`PENDING` / `APPROVED` / `REJECTED` / `WITHDRAWN` / `CANCELLED`） */
  status: string;
  /** 实例状态中文名（后端给，前端不再维护第二份映射） */
  status_label: string;
  /** 还在等几个人批（非 PENDING 恒 0） */
  pending_tasks: number;
}

/** 单头 + 明细出参（后端 `QuotationRes`） */
export interface Quotation {
  /** 主键 */
  id: string;
  /** 单号 `QT-yyyyMMdd-000001` */
  quote_no: string;
  /** 客户 id */
  customer_id: string;
  /** 客户编码（软删客户 → null） */
  customer_code: string | null;
  /** 客户名称（软删客户 → null） */
  customer_name: string | null;
  /** 报价日期 `yyyy-MM-dd` */
  quote_date: string;
  /** 有效期至（含当天） */
  valid_until: string;
  /** 是否已过期（后端现算 `today > valid_until`） */
  expired: boolean;
  /** 状态 */
  status: QuotationStatus;
  /** 状态中文名 */
  status_label: string;
  /** 不含税合计 */
  total_amount: DecimalText;
  /** 税额合计 */
  total_tax: DecimalText;
  /** 价税合计 */
  total_with_tax: DecimalText;
  /** 转成的销售订单 id（null = 未转单） */
  converted_so_id: string | null;
  /** 转单时间（RFC3339） */
  converted_at: string | null;
  /** 最近一轮审批实例 id（null = 从没提交过审批） */
  approval_instance_id: string | null;
  /** 审批摘要（**只有详情页有**） */
  approval: QuotationApproval | null;
  /** 备注 */
  remark: string | null;
  /** 现在能不能改（= DRAFT） */
  editable: boolean;
  /** 现在能不能删（= DRAFT / CANCELLED） */
  deletable: boolean;
  /** 现在能流转到哪些状态（前端按它显示按钮；空 = 终态） */
  allowed_targets: QuotationStatus[];
  /** 明细（列表页为空数组，详情页带全量） */
  items: QuotationLine[];
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
  /** 创建人（uuid） */
  create_by: string | null;
  /** 更新人（uuid） */
  update_by: string | null;
}

/** 分页筛选（逐字抄 `QuotationFilter`） */
export interface QuotationPageQuery {
  /** 单号模糊 */
  quote_no?: string;
  /** 状态精确（5 值之一） */
  status?: QuotationStatus | '';
  /** 客户精确 */
  customer_id?: string;
  /** 有效期：true = 只要已过期、false = 只要未过期（含今天）、不传 = 全部 */
  expired?: boolean;
  /** 页码（1 起） */
  page?: number;
  /** 每页条数（0 = 20，上限 200） */
  page_size?: number;
}

/** 明细入参（`QuotationLineReq`）：**金额不传**，后端按 单价 × 数量 现算 */
export interface QuotationLineReq {
  /** 物料 id */
  material_id: string;
  /** 数量（> 0） */
  qty?: DecimalText;
  /** 不含税单价（≥ 0，留空 = 0） */
  price?: DecimalText;
  /** 税率百分数（0–100，留空 = 0） */
  tax_rate?: DecimalText;
  /** 行备注 */
  remark?: string | null;
}

/** 新建报价单（`CreateQuotationReq`） */
export interface CreateQuotationReq {
  /** 客户 id（必须存在、未软删且 ACTIVE） */
  customer_id: string;
  /** 报价日期（留空 = 今天 CST） */
  quote_date?: string;
  /** 有效期至（**必填**，`>= quote_date`） */
  valid_until: string;
  /** 备注 */
  remark?: string | null;
  /** 明细（至少 1 行，最多 200 行） */
  items: QuotationLineReq[];
}

/** 编辑报价单（`UpdateQuotationReq`：**不传 = 不改**，明细传了 = 整单替换） */
export interface UpdateQuotationReq {
  customer_id?: string;
  quote_date?: string;
  valid_until?: string;
  remark?: string | null;
  items?: QuotationLineReq[];
}

/** 状态流转（`QuotationStatusReq`） */
export interface QuotationStatusReq {
  /** 目标状态（必须是 `LEGAL_EDGES` 里的边） */
  target_status: QuotationStatus;
  /** 原因（客户拒绝 / 已发出后作废必填） */
  reason?: string;
}

/** 提交审批入参（可以完全不带：`skip_*` 只在「没配流程」时生效） */
export interface QuotationSubmitReq {
  /** 跳过审批（需要 `business:erp:skip-approval`） */
  skip_approval?: boolean;
  /** 跳过理由（≥ 10 字） */
  skip_reason?: string;
}

/** 转销售订单入参（`ConvertQuotationReq`；全部可选，空 body 也行） */
export interface ConvertQuotationReq {
  /** 下单日期（留空 = 今天 CST；**不是**报价日期） */
  order_date?: string;
  /** 要求交期（不能早于 order_date） */
  delivery_date?: string | null;
  /** 销售员 id（留空 = 不指定） */
  salesperson_id?: string | null;
  /** 备注（后端会把「由报价单 QT-… 转换」拼在前面，来源不会被覆盖） */
  remark?: string | null;
}

/** 转单出参的单头段 */
export interface ConvertedOrderHeader {
  /** 新销售订单 id */
  so_id: string;
  /** 新销售订单号 `SO-yyyyMMdd-000001` */
  so_no: string;
  /** 订单状态（新建 = DRAFT） */
  status: string;
  /** 状态中文名 */
  status_label: string;
  /** 客户 id */
  customer_id: string;
  /** 客户编码 */
  customer_code: string | null;
  /** 客户名称 */
  customer_name: string | null;
  /** 下单日期 */
  order_date: string;
  /** 要求交期 */
  delivery_date: string | null;
  /** 销售员 id */
  salesperson_id: string | null;
  /** 销售员姓名 */
  salesperson_name: string | null;
  /** 不含税合计 */
  total_amount: DecimalText;
  /** 税额合计 */
  total_tax: DecimalText;
  /** 价税合计 */
  total_with_tax: DecimalText;
  /** 原币币种 */
  currency_code: string;
  /** 建单锁定的汇率（本位币单恒 1） */
  exchange_rate: DecimalText;
  /** 本位币不含税金额 */
  base_total_amount: DecimalText;
  /** 本位币税额 */
  base_total_tax: DecimalText;
  /** 本位币价税合计 */
  base_total_with_tax: DecimalText;
  /** 订单备注 */
  remark: string | null;
}

/** 转单出参的明细段 */
export interface ConvertedOrderLine {
  /** 行号（与报价单行号一一对应） */
  line_no: number;
  /** 物料 id */
  material_id: string;
  /** 物料编码 */
  material_code: string | null;
  /** 物料名称 */
  material_name: string | null;
  /** 规格 */
  spec: string | null;
  /** 计量单位名 */
  unit_name: string | null;
  /** 数量（原样搬自报价行） */
  qty: DecimalText;
  /** 不含税单价（原样搬自报价行） */
  price: DecimalText;
  /** 行不含税金额 */
  amount: DecimalText;
  /** 税率百分数 */
  tax_rate: DecimalText;
  /** 行税额 */
  tax_amount: DecimalText;
}

/** 转单出参（`data.header` / `data.items` / `data.meta` 三段式） */
export interface ConvertQuotationRes {
  /** 新销售订单单头 */
  header: ConvertedOrderHeader;
  /** 新销售订单明细 */
  items: ConvertedOrderLine[];
  /** 附属信息：`quotation_id` / `quote_no` / `already_converted` / `line_count` … */
  meta: DocMeta & {
    /** 报价单 id（回链） */
    quotation_id?: string;
    /** 报价单号 */
    quote_no?: string;
    /** 是否幂等命中（true = 这张单之前已转过，本次没建新订单） */
    already_converted?: boolean;
    /** 搬过去的行数 */
    line_count?: number;
  };
}

/** 空值整键缺席（后端把 `?status=` 当「等于空串」过滤，会查出 0 行） */
function asParams<T extends object>(query: T): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === '') {
      continue;
    }
    out[key] = value;
  }
  return out;
}

/** 报价单分页（`GET /business/erp/quotation/page`） */
export function fetchQuotationPage(query: QuotationPageQuery): Promise<PageData<Quotation>> {
  return get<PageData<Quotation>>(`${BASE}/quotation/page`, asParams(query));
}

/** 报价单详情（`GET /business/erp/quotation/{id}`；软删行 404） */
export function fetchQuotationDetail(id: string): Promise<Quotation> {
  return get<Quotation>(`${BASE}/quotation/${id}`);
}

/** 新建报价单（`POST /business/erp/quotation`，权限 `:create`） */
export function createQuotation(payload: CreateQuotationReq): Promise<Quotation> {
  return post<Quotation>(`${BASE}/quotation`, payload);
}

/** 编辑报价单（`PUT /business/erp/quotation/{id}`，权限 `:update`，只有 DRAFT） */
export function updateQuotation(id: string, payload: UpdateQuotationReq): Promise<Quotation> {
  return put<Quotation>(`${BASE}/quotation/${id}`, payload);
}

/**
 * 状态流转（`PATCH /business/erp/quotation/{id}/status`，权限 `:status`）。
 *
 * ⚠️ `DRAFT → SENT` **不要直接往这里传**：那是「把价格发给客户」的对外承诺，
 * 必须有 APPROVED 的审批实例，否则 409 + `66088`。发出去走 {@link submitQuotationApproval}。
 */
export function changeQuotationStatus(id: string, payload: QuotationStatusReq): Promise<Quotation> {
  return patch<Quotation>(`${BASE}/quotation/${id}/status`, payload);
}

/** 软删报价单（`DELETE /business/erp/quotation/{id}`，权限 `:delete`，只有 DRAFT / CANCELLED） */
export function deleteQuotation(id: string): Promise<void> {
  return del<void>(`${BASE}/quotation/${id}`);
}

/**
 * 提交审批（`POST /business/erp/quotation/{id}/submit-approval`，权限 `:status`）。
 *
 * - 发起 `quotation_approve` 实例后单据**仍是 `DRAFT`**（审批通过由流程引擎回调自动发出）；
 * - 没配流程时只有持 `business:erp:skip-approval` 且理由 ≥10 字才能 `skip_approval` 直接发出；
 * - 在跑实例 / 已通过未发出 / 明细为空 / 客户冻结 / 有效期已过 → 409，报文点名缺什么。
 */
export function submitQuotationApproval(
  id: string,
  payload: QuotationSubmitReq = {},
): Promise<Quotation> {
  return post<Quotation>(`${BASE}/quotation/${id}/submit-approval`, payload);
}

/**
 * 转销售订单（`POST /business/erp/quotation/{id}/convert`，权限 `:convert`）。
 *
 * 只有 `ACCEPTED` 能转；明细整列搬、金额不重算；已转过 → 幂等回原订单
 * （`meta.already_converted === true`，不会建第二张）。
 */
export function convertQuotation(id: string, payload: ConvertQuotationReq = {}): Promise<ConvertQuotationRes> {
  return post<ConvertQuotationRes>(`${BASE}/quotation/${id}/convert`, payload);
}

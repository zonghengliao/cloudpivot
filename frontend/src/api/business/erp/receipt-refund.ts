/**
 * 收款退款单 API 层（后端 `backend/src/business/erp/receipt_refund.rs`，
 * SO-14 遗留卡 t_573bfe5b 建表 → 本卡 NEAR-FIX-1 / t_67dcb9cb 补前端）。
 *
 * # 为什么单独一个文件（不并进 `erp/index.ts`）
 *
 * 与 `sales-return.ts` / `shipment.ts` 同一口径：`index.ts` 已是数千行的总契约，
 * 退款单是独立后端模块（自己的 handler / model / 迁移 V0294 / V0295 / V0297 / V0361），
 * 按模块边界单独成文件。`index.ts` 一行不动。
 *
 * # 字段名逐字抄后端（不是猜的）
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 单号 | `refund_no`（`RR-yyyyMMdd-000001`） | `receipt_refund::NO_PREFIX` |
 * | 状态 | `status` + `status_label` | 后端给中文名，前端不再维护第二份译名 |
 * | 金额 | `amount` / `allocated_amount` / `unallocated_amount` | 恒等 `allocated ≤ amount`（`ck_biz_receipt_refund_amount`） |
 * | 审批 | `approval_instance_id` / `approval_instance_no` / `approval_status` | 列表用 JOIN 带出，详情用 `approval` 段 |
 * | 冲减明细 | `allocations[]`（只在 `GET /{id}` 返回） | `RefundDetailRes::allocations` |
 *
 * # 五个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端路由宏不含前缀，漏了会命中 SPA fallback 返回 HTML
 *    → 前端报「响应体格式不符合」）；
 * 2. **审批不是一个状态**：`status` 只有 DRAFT / CONFIRMED / POSTED / CANCELLED，
 *    「已提交待批」要读 `approval_status === 'PENDING'`（后端模块文档第 71-74 行明说）。
 *    所以「撤回」按钮的显示判据是 `approval_status === 'PENDING'`，不是 `status`；
 * 3. **过账要求核销满**（`allocated_amount === amount`），否则 409 + 明确文案；
 * 4. **冲减明细没有「反核销」接口**：`allocate` 只 UPSERT，不删不在本次 items 里的行
 *    （与收款单同款），所以已冲减的行只能改大、不能取消；
 * 5. **退款原因必填且 ≥ 4 字**（`receipt_refund::REASON_MIN_CHARS`，库层 CHECK 双闸）——
 *    前端要在提交前拦，不能等 400。
 *
 * # 撤回走的是流程引擎，不是本模块
 *
 * `POST /business/flow/instance/{id}/withdraw`（`api/business/flow-api.ts::withdrawFlowInstance`）。
 * 本文件**不重复包一层**，页面直接 import 那个函数 —— 两处各写一遍必然漂移。
 */
import { get, patch, post, type PageData } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/**
 * 退款单状态域（4 值，逐字抄 `receipt_refund::STATUSES` / `ck_biz_receipt_refund_status`）：
 *
 * `DRAFT → CONFIRMED → POSTED`（过账生成退款凭证 借 1122 / 贷 1002），
 * `DRAFT → CANCELLED`（只有草稿能作废）。
 *
 * **POSTED 是终态**：钱已付出且凭证已记账，不能作废 —— 要抹掉只能由客户重新回一张
 * 收款单，或在凭证层红冲。与收款单「CONFIRMED → CANCELLED 也没有边」同一套理由。
 */
export type RefundStatus = 'DRAFT' | 'CONFIRMED' | 'POSTED' | 'CANCELLED';

/** 退款方式域（4 值，逐字抄 `receipt_refund::METHODS` / `ck_biz_receipt_refund_method`） */
export type RefundMethod = 'BANK_TRANSFER' | 'CASH' | 'CHECK' | 'OTHER';

/**
 * 审批状态域（`biz_flow_instance.status` 5 值）。
 *
 * `null`（而不是 `'NONE'`）= **从未提交过审批** —— 后端不给这个值编造一个码，
 * 前端也不要自己造。
 */
export type RefundApprovalStatus = 'PENDING' | 'APPROVED' | 'REJECTED' | 'WITHDRAWN' | 'CANCELLED';

/** 退款单行（后端 `receipt_refund::RefundRes`，列表 / 详情共用） */
export interface ReceiptRefund {
  id: string;
  /** 退款单号 `RR-yyyyMMdd-000001` */
  refund_no: string;
  customer_id: string;
  /** 客户名称（后端 LEFT JOIN `biz_customer`；客户软删时是空串） */
  customer_name: string;
  /** 来源收款单 id（可选，弱引用） */
  source_receipt_id: string | null;
  /**
   * 来源收款单号（`RC-yyyyMMdd-000001`）。
   *
   * 后端 `RefundRes::source_receipt_no`（V0368 补的列：LEFT JOIN `biz_receipt`）。
   * `null` = 没选来源收款单 / 收款单已软删 —— 界面显示「—」，不要拿 id 顶。
   */
  source_receipt_no: string | null;
  /** 退款日期 `yyyy-MM-dd` */
  refund_date: string;
  /** 退款金额（`Decimal` 出 JSON 是字符串，如 `"100.0000"`） */
  amount: number | string;
  /** 已冲减金额 */
  allocated_amount: number | string;
  /** 未冲减余额（= amount − allocated_amount） */
  unallocated_amount: number | string;
  method: RefundMethod | string | null;
  /** 退款方式中文（后端 `receipt_refund::method_label`，未指定时「未指定」） */
  method_label: string;
  bank_account: string | null;
  status: RefundStatus | string;
  /** 状态中文（后端 `receipt_refund::status_label`） */
  status_label: string;
  /** 退款凭证 id（未过账为 null） */
  voucher_id: string | null;
  /** 最近一次提交的审批实例 id（null = 从未提交过审批） */
  approval_instance_id: string | null;
  /** 审批单号（`FLW-yyyyMMdd-XXXXXXXX`） */
  approval_instance_no: string | null;
  /** 审批状态（**「已提交待批」的唯一判据**，不能看 status） */
  approval_status: RefundApprovalStatus | string | null;
  /** 审批状态中文 */
  approval_status_label: string | null;
  /** 退款原因（建单必填，≥ 4 字） */
  reason: string;
  remark: string | null;
  created_at: string | null;
  updated_at: string | null;
}

/** 冲减明细（后端 `receipt_refund::RefundAllocationRes`） */
export interface ReceiptRefundAllocation {
  id: string;
  receivable_id: string;
  /** 应收单号 `AR-yyyyMMdd-000001` */
  ar_no: string;
  /** 应收金额 */
  ar_amount: number | string;
  /** 应收已收（**冲减后**） */
  ar_received: number | string;
  /** 应收未收（**冲减后**） */
  ar_unsettled: number | string;
  ar_status: string;
  ar_status_label: string;
  /** 本次冲减金额（正数 = 本次冲减掉多少已收款） */
  amount: number | string;
  remark: string | null;
  created_at: string | null;
}

/** 审批进度摘要（后端 `receipt_refund::RefundApprovalRes`；只给**最近一条**实例） */
export interface ReceiptRefundApproval {
  instance_id: string;
  instance_no: string;
  status: RefundApprovalStatus | string;
  /** 还在等几个人批（非 PENDING 恒 0） */
  pending_tasks: number;
}

/** 退款单详情（`ReceiptRefund` 展开 + 凭证号 + 冲减明细 + 审批进度） */
export interface ReceiptRefundDetail extends ReceiptRefund {
  /** 凭证号（未过账 → 空串，不是 null） */
  voucher_no: string;
  allocations: ReceiptRefundAllocation[];
  /** 最近一条审批实例（从没提交过审批 → null） */
  approval: ReceiptRefundApproval | null;
}

/**
 * 分页入参（逐字抄 `receipt_refund::RefundPageQuery`）。
 *
 * `refund_no` 是**模糊**匹配（单号 + 客户名），不是 `keyword`；
 * 空串一律不发（`asParams` 兜住）—— 后端把 `?refund_no=` 当「等于空串」过滤会查出 0 行。
 */
export interface ReceiptRefundQuery {
  page?: number;
  page_size?: number;
  status?: RefundStatus | '';
  customer_id?: string;
  refund_no?: string;
  method?: RefundMethod | '';
  /** 退款日期起（含）`yyyy-MM-dd` */
  date_from?: string;
  /** 退款日期止（含）`yyyy-MM-dd` */
  date_to?: string;
}

/** 登记退款请求（后端 `receipt_refund::CreateRefundReq`） */
export interface CreateReceiptRefundReq {
  /** 客户 id（必填） */
  customer_id: string;
  /** 来源收款单 id（可选：退的是哪一笔收款） */
  source_receipt_id?: string | null;
  /** 退款日期 `yyyy-MM-dd`（留空 = 今天 CST） */
  refund_date?: string | null;
  /** 退款金额（必填，> 0） */
  amount: number | string;
  method?: RefundMethod | null;
  bank_account?: string | null;
  /** 退款原因（**必填**，≥ 4 字） */
  reason: string;
  remark?: string | null;
}

/** 一条冲减明细（后端 `receipt_refund::RefundAllocateLineReq`） */
export interface AllocateRefundItemReq {
  receivable_id: string;
  /** 本次冲减金额（> 0，且 ≤ 该应收的「可退金额」） */
  amount: number | string;
  remark?: string | null;
}

/** 状态迁移请求（后端 `receipt_refund::RefundStatusReq`） */
export interface ChangeRefundStatusReq {
  status: RefundStatus;
  /** 作废必填（≥ 4 字） */
  reason?: string | null;
}

/** 空串 / null / undefined 一律不发（后端把 `?x=` 当「等于空串」过滤） */
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

/** 退款单分页（`GET /business/erp/receipt-refund/page`，权限 `business:erp:receipt-refund:list`） */
export function fetchReceiptRefundPage(
  query: ReceiptRefundQuery = {},
): Promise<PageData<ReceiptRefund>> {
  return get<PageData<ReceiptRefund>>(`${BASE}/receipt-refund/page`, asParams(query));
}

/**
 * 退款单详情（`GET /business/erp/receipt-refund/{id}`，权限 `:detail`）。
 *
 * 详情比列表多三段：`voucher_no` / `allocations[]` / `approval`。
 */
export function fetchReceiptRefundDetail(id: string): Promise<ReceiptRefundDetail> {
  return get<ReceiptRefundDetail>(`${BASE}/receipt-refund/${id}`);
}

/** 登记退款（`POST /business/erp/receipt-refund`，权限 `:create`）→ 返回详情（状态 DRAFT） */
export function createReceiptRefund(
  payload: CreateReceiptRefundReq,
): Promise<ReceiptRefundDetail> {
  return post<ReceiptRefundDetail>(`${BASE}/receipt-refund`, payload);
}

/**
 * 退款核销（`POST /business/erp/receipt-refund/{id}/allocate`，权限 `:allocate`）。
 *
 * 语义是**冲减应收已收金额**（`received_amount -= 金额`），不是新增核销行；
 * 同一 (退款单, 应收) 只有一行，重复提交 = 改金额（UPSERT）。
 */
export function allocateReceiptRefund(
  id: string,
  allocations: AllocateRefundItemReq[],
): Promise<ReceiptRefundDetail> {
  return post<ReceiptRefundDetail>(`${BASE}/receipt-refund/${id}/allocate`, { allocations });
}

/**
 * 状态迁移（`PATCH /business/erp/receipt-refund/{id}/status`，权限 `:status`）。
 *
 * 三条边：`DRAFT → CONFIRMED`（确认）、`CONFIRMED → POSTED`（过账，**要求核销满**）、
 * `DRAFT → CANCELLED`（作废，原因 ≥ 4 字）。非法迁移 409。
 */
export function changeReceiptRefundStatus(
  id: string,
  payload: ChangeRefundStatusReq,
): Promise<ReceiptRefundDetail> {
  return patch<ReceiptRefundDetail>(`${BASE}/receipt-refund/${id}/status`, payload);
}

/**
 * 提交审批（`POST /business/erp/receipt-refund/{id}/submit-approval`，权限 `:status`）。
 *
 * 前置：`status = CONFIRMED` 且核销满（后端 409 + 明确文案）。
 * 审批通过后**自动过账**（回调走 `post_after_approval`）；驳回 / 撤回则仍是 CONFIRMED
 * （账一分没动，可改完再提）。
 *
 * `body` 可省略 —— 后端签名是 `Option<Json<RefundSubmitReq>>`。
 */
export function submitReceiptRefundApproval(
  id: string,
  body: { reason?: string | null } = {},
): Promise<ReceiptRefundDetail> {
  return post<ReceiptRefundDetail>(`${BASE}/receipt-refund/${id}/submit-approval`, body);
}

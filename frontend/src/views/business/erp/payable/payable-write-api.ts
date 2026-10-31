/**
 * 应付付款 / 作废的 API（P7）。
 *
 * # 为什么不往 `@/api/business/erp/index.ts` 里加
 *
 * 那个文件是**全 ERP 共用**的契约文件（2230 行），批次 2 里 P4 / P5 / P6 多张卡同时在改它
 * （并行卡刚往里面加过即时库存页的接口）。本卡只往 `payable/` + `voucher/` 两个页面目录
 * 里加文件，避免和别人在同一段 diff 上打架。
 *
 * 契约仍然**逐字对齐后端**：
 *
 * * `POST /business/erp/payable/{id}/pay`    → `PayableDetailRes`
 * * `POST /business/erp/payable/{id}/cancel` → `PayableDetailRes`
 *
 * `PayableDetailRes` 在 Rust 侧是 `#[serde(flatten)] payable + voucher_no + payments`，
 * 所以 JSON 顶层就是「应付单字段 + voucher_no + payments」——见 {@link PayableDetail}。
 */
import { get, post } from '@/api/request';
import type { PayableRow } from '@/api/business/erp';

/** 后端路径前缀（后端注解不含 `/api`，前端写全路径）。 */
const BASE = '/api/business/erp';

/** 一条付款记录（`payable_payment.rs::PaymentRes`）。 */
export interface PayablePayment {
  /** 付款记录 id */
  id: string;
  /** 付款单号 `PM-yyyyMMdd-000001` */
  payment_no: string;
  /** 所属应付单 id */
  payable_id: string;
  /** 本次付款金额（`Decimal` → JSON 字符串） */
  amount: number | string;
  /** 付款日期 `YYYY-MM-DD` */
  pay_date: string;
  /** 付款方式 */
  pay_method: string | null;
  /** 付款凭证 id */
  voucher_id: string | null;
  /** 付款凭证号 */
  voucher_no: string | null;
  /** 备注 */
  remark: string | null;
  /** 登记时间 */
  created_at: string;
  /**
   * 审批状态（P17）：`NONE` 未配流程/直付生效 | `PENDING` 审批中（**账未动**）|
   * `APPROVED` 已通过 | `REJECTED` 已驳回 | `CANCELLED` 已终止
   */
  approval_status: string;
  /** 审批状态中文标签（后端给，前端不再自己映射一份） */
  approval_status_label: string;
  /** 审批实例 id（有流程时才有；详情抽屉据此显示审批进度） */
  approval_instance_id: string | null;
  /** 是否尚未生效（`approval_status === 'PENDING'`） */
  pending: boolean;
}

/**
 * 应付详情（`PayableDetailRes`）：单头 flatten + 关联凭证号 + 付款记录时间线。
 *
 * `voucher_no` 是**这张应付单的进项凭证号**（收货过账生成），不是付款凭证。
 */
export interface PayableDetail extends PayableRow {
  /** 进项凭证号（`voucher_id` 为空或凭证已删时是 null） */
  voucher_no: string | null;
  /** 付款记录（`pay_date` 升序；没付过款是空数组） */
  payments: PayablePayment[];
}

/** 登记付款入参。 */
export interface PayPayableReq {
  /** 本次付款金额（后端 `numeric(18,4)`，用字符串传避免浮点丢精度） */
  amount: string;
  /** 付款日期 `YYYY-MM-DD`（null = 今天） */
  pay_date?: string | null;
  /** 付款方式 */
  pay_method?: string | null;
  /** 备注 */
  remark?: string | null;
}

/**
 * 提交付款的结果（`payable_payment.rs::PaySubmitRes`）。
 *
 * * `outcome = 'PENDING_APPROVAL'`（HTTP **202**）：已发起审批，**账还没动**；
 * * `outcome = 'PAID'`（HTTP 200）：没配审批流程，按 P7 老行为直接生效。
 */
export interface PaySubmitResult {
  /** `PENDING_APPROVAL` / `PAID` */
  outcome: string;
  /** 可直接提示给用户的一句话（前端不用自己拼） */
  message: string;
  /** 本次付款金额 */
  amount: number | string;
  /** 付款记录 id */
  payment_id: string | null;
  /** 付款单号 */
  payment_no: string | null;
  /** 审批实例 id（未配流程时为 null） */
  approval_instance_id: string | null;
  /** 应付单最新详情（含全部付款记录） */
  detail: PayableDetail;
}

/** 应付详情（含付款记录）。 */
export function fetchPayableDetail(id: string): Promise<PayableDetail> {
  return get<PayableDetail>(`${BASE}/payable/${id}`);
}

/**
 * 提交一笔付款。
 *
 * P17 起：配了「应付付款审批」流程 → **只发起审批**（流水不记账，202）；
 * 没配流程 → 降级直付（立即生成「借 2202 / 贷 1002」的凭证，200）。
 * 前端按返回的 `outcome` 区分提示，不要假设「提交了就付款成功」。
 */
export function payPayable(id: string, body: PayPayableReq): Promise<PaySubmitResult> {
  return post<PaySubmitResult>(`${BASE}/payable/${id}/pay`, body);
}

/**
 * 作废应付单（仅 OPEN 且未付款）。
 *
 * 后端校验：只有「未付且一分未付」才允许（否则 409）、`reason` 必填（否则 400）。
 * 作废会同时红冲这张应付单的进项凭证。
 */
export function cancelPayable(id: string, reason: string): Promise<PayableDetail> {
  return post<PayableDetail>(`${BASE}/payable/${id}/cancel`, { reason });
}

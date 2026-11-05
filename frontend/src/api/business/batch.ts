/**
 * 批量操作接口契约（D7）。
 *
 * 八个核心列表页共用这一份：**路径写全 `/api/...`**（漏前缀会命中 SPA fallback 返回 HTML，
 * 报「响应体格式不符合」—— 本仓库最常见的一类假故障）。
 *
 * # 统一响应
 *
 * ```json
 * {
 *   "succeeded": [{ "id": "...", "no": "IN-2026..." }],
 *   "failed":    [{ "id": "...", "no": "IN-2026...", "code": 10003, "error": "状态已是已过账" }],
 *   "total": 10, "success_count": 8, "fail_count": 2
 * }
 * ```
 *
 * 批量接口**不会**因为某条失败而整批 4xx/5xx：只有「请求本身不合法」（空 ids、超上限、
 * 非法 UUID、缺权限）才是非 2xx。所以前端拿到 2xx 就把 `failed` 逐条展示给用户。
 *
 * # 导出类
 *
 * `batch-export` 返回的是**异步导出任务回执**（不是行数据）：提示用户去「导入导出」页下载。
 */

import { post } from '@/api/request';
import type { BatchExportTask, BatchResult } from '@/utils/batch-action';

/** 批量状态流转入参（单据类共享）。 */
export interface BatchStatusPayload {
  /** 勾选的主键。 */
  ids: string[];
  /** 目标状态（各资源白名单由后端判）。 */
  status?: string;
  /** 原因（高危动作必填）。 */
  reason?: string;
}

/** 批量导出入参。 */
export interface BatchExportPayload {
  /** 勾选的主键；空 = 按 `keyword` / `status_code` 导「当前筛选结果」。 */
  ids: string[];
  /** 关键词（与列表页同一口径）。 */
  keyword?: string;
  /**
   * 启用 / 停用过滤（整型 1 启用 / 0 停用）。
   *
   * 只给「字典项 / 用户」这类**开关型状态**的定义用；ERP 单据
   * （`erp.stock_in` / `erp.payable` / `erp.voucher`）的状态是文本码，
   * 传 `status` 会被后端 **422 + 10001** 拒掉（并提示改用 `status_code`），
   * 所以那三张单据页**永远不传这个键**。
   */
  status?: number;
  /**
   * 单据状态码（文本：`POSTED` / `SETTLED` / `CANCELLED` …）。
   *
   * 值与列表页 `filter.status` **同一个值域**（后端白名单就是导出列里那份
   * `EnumItem`，见 `impexp::builtin::erp_doc` 的 `status_codes()`）；
   * 不区分大小写，非法码 → 422 + 10001，`message` 点名该 biz_type 的全部合法码。
   * 空串 = 不过滤：前端直接**不发这个键**（不发比发空串更不容易漂移）。
   */
  status_code?: string;
}

/** 批量审批入参。 */
export interface BatchApprovePayload {
  /** 勾选的待办 id。 */
  ids: string[];
  /** `APPROVE` / `REJECT`。 */
  decision: 'APPROVE' | 'REJECT';
  /** 审批意见（驳回必填 ≥ 5 字）。 */
  comment?: string;
  /** 驳回退回目标（`PREV` 默认）。 */
  return_to?: string;
}

/** 批量改状态入参。 */
export interface BatchTaskStatusPayload {
  /** 勾选的任务 id。 */
  ids: string[];
  /** 目标状态。 */
  status: string;
  /** 原因（打回必填）。 */
  reason?: string;
}

/** 批量指派入参。 */
export interface BatchTaskAssignPayload {
  /** 勾选的任务 id。 */
  ids: string[];
  /** 新负责人。 */
  owner_id: string;
  /** 转派原因。 */
  reason?: string;
}

/** 收货入库单：批量过账 / 作废。 */
export function stockInBatchStatus(payload: BatchStatusPayload): Promise<BatchResult> {
  return post<BatchResult>('/api/business/erp/stock-in/batch-status', payload);
}

/** 收货入库单：批量删除。 */
export function stockInBatchDelete(ids: string[]): Promise<BatchResult> {
  return post<BatchResult>('/api/business/erp/stock-in/batch-delete', { ids });
}

/** 收货入库单：批量导出（异步任务）。 */
export function stockInBatchExport(payload: BatchExportPayload): Promise<BatchExportTask> {
  return post<BatchExportTask>('/api/business/erp/stock-in/batch-export', payload);
}

/** 采购订单：批量状态流转（`SUBMITTED` 走审批入口）。 */
export function purchaseOrderBatchStatus(payload: BatchStatusPayload): Promise<BatchResult> {
  return post<BatchResult>('/api/business/erp/purchase-order/batch-status', payload);
}

/** 调拨单：批量状态流转。 */
export function transferBatchStatus(payload: BatchStatusPayload): Promise<BatchResult> {
  return post<BatchResult>('/api/business/erp/transfer/batch-status', payload);
}

/** 序列号：批量状态流转（出库 / 报废）。 */
export function serialBatchStatus(payload: BatchStatusPayload): Promise<BatchResult> {
  return post<BatchResult>('/api/business/erp/serial/batch-status', payload);
}

/** 应付单：批量导出（异步任务）。 */
export function payableBatchExport(payload: BatchExportPayload): Promise<BatchExportTask> {
  return post<BatchExportTask>('/api/business/erp/payable/batch-export', payload);
}

/** 凭证：批量导出（异步任务）。 */
export function voucherBatchExport(payload: BatchExportPayload): Promise<BatchExportTask> {
  return post<BatchExportTask>('/api/business/erp/voucher/batch-export', payload);
}

/** 审批中心：批量同意 / 驳回。 */
export function flowTaskBatchApprove(payload: BatchApprovePayload): Promise<BatchResult> {
  return post<BatchResult>('/api/business/flow/task/batch-approve', payload);
}

/** 任务：批量改状态。 */
export function taskBatchStatus(payload: BatchTaskStatusPayload): Promise<BatchResult> {
  return post<BatchResult>('/api/business/task/batch-status', payload);
}

/** 任务：批量指派。 */
export function taskBatchAssign(payload: BatchTaskAssignPayload): Promise<BatchResult> {
  return post<BatchResult>('/api/business/task/batch-assign', payload);
}

/**
 * 联动作废 API（ARCH-3 §6 R-P1-5 · 断链 DC-24）。
 *
 * # 两个端点
 *
 * | 端点 | 作用 |
 * |---|---|
 * | `GET  /business/erp/{doc_type}/{id}/cancel-impact`  | 影响分析（只读）：会波及哪些下游、每张怎么处理、有没有拦路虎 |
 * | `POST /business/erp/{doc_type}/{id}/cancel-cascade` | 联动作废：一个事务按顺序处理，任一步失败整笔回滚 |
 *
 * **前端必须先调 impact 把影响面摆给操作员看**，点「一键按顺序处理」之后才调 cascade ——
 * 后端刻意不做「点作废就顺手改下游」的静默级联。
 *
 * # 路径必须写全 `/api/...`
 *
 * 后端路由宏里的路径不含 `/api`（由 `[router].prefix` 注入），漏前缀会命中 SPA fallback
 * 返回 `index.html`，报「响应体格式不符合统一响应规范」。
 *
 * # 权限码
 *
 * - 影响分析：`business:erp:doc:cancel-impact`
 * - 联动作废：`business:erp:doc:cancel-cascade`
 */

import { get, post } from '@/api/request';

/** 支持联动作废的单据类型（后端 `cancel_cascade::DOC_TYPE_*`）。 */
export type CancelDocType = 'purchase-order' | 'stock-in' | 'payable';

/** 影响面里的一行。 */
export interface CancelImpactItem {
  /** 执行序号（1 起）：同号的几条在同一个事务步骤里一起做 */
  exec_step: number;
  /** 单据类型：`purchase-order` / `stock-in` / `voucher` / `payable` */
  doc_type: string;
  /** 类型中文名 */
  doc_type_label: string;
  /** 单据 id */
  doc_id: string;
  /** 单号 */
  doc_no: string;
  /** 当前状态 */
  doc_status: string;
  /** 状态中文名 */
  doc_status_label: string;
  /** 金额（字符串定点，别用 Number 直接比） */
  amount: string;
  /** 处理方式：`EXECUTE` / `SKIP` / `BLOCKED` */
  action: 'EXECUTE' | 'SKIP' | 'BLOCKED';
  /** 处理方式的一句话说明 */
  action_label: string;
  /** 补充说明 */
  detail: string;
}

/** 影响分析出参。 */
export interface CancelImpactRes {
  doc_type: string;
  doc_id: string;
  doc_no: string;
  doc_status: string;
  doc_status_label: string;
  amount: string;
  /** 能不能一键处理（false 时前端只允许「取消」） */
  can_cancel: boolean;
  /** 弹窗标题用的一句话 */
  summary: string;
  steps: CancelImpactItem[];
  /** 拦路虎（空 = 可以一键处理） */
  blockers: string[];
  /** 固定提示（直接显示，别自己拼） */
  note: string;
}

/** 联动作废里一步的执行结果。 */
export interface CancelCascadeStep {
  seq: number;
  doc_type: string;
  doc_no: string;
  outcome: 'DONE' | 'SKIPPED';
  message: string;
}

/** 联动作废出参。 */
export interface CancelCascadeRes {
  doc_type: string;
  doc_no: string;
  steps: CancelCascadeStep[];
  message: string;
  transaction: string;
}

/** 影响分析（只读，点「作废」时先调它）。 */
export function fetchCancelImpact(
  docType: CancelDocType,
  id: string,
): Promise<CancelImpactRes> {
  return get<CancelImpactRes>(`/api/business/erp/${docType}/${id}/cancel-impact`);
}

/** 联动作废（一个事务；`reason` 必填 ≥4 字）。 */
export function postCancelCascade(
  docType: CancelDocType,
  id: string,
  reason: string,
): Promise<CancelCascadeRes> {
  return post<CancelCascadeRes>(`/api/business/erp/${docType}/${id}/cancel-cascade`, {
    reason,
  });
}

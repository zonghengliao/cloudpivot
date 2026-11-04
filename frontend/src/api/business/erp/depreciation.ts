/**
 * 折旧计提 API 层（后端 `depreciation_handler.rs` / `asset_dto.rs` 的折旧半区）。
 *
 * 词表（批次状态 / 计提结果）在 `./asset-kinds`（零 import），这里只转出。
 *
 * # 四条端点（路径必须带 `/api`）
 *
 * | 方法 | 路径 | 权限码 |
 * |------|------|--------|
 * | GET  | `/api/business/erp/depreciation/page` | `business:erp:depreciation:list` |
 * | GET  | `/api/business/erp/depreciation/{id}` | 同上 |
 * | POST | `/api/business/erp/depreciation/run` | `business:erp:depreciation:run` |
 * | POST | `/api/business/erp/depreciation/{id}/reverse` | `business:erp:depreciation:reverse` |
 *
 * # 三条口径
 *
 * 1. **手动与定时是同一个 `run_batch`**（每月 1 日 01:30 的 `sys_job erp_depreciation_run` 也走它），
 *    所以页面上的「计提」按钮不该有第二套算法 —— 前端只发期间，金额全看回包；
 * 2. `run` 的回包是 `outcome` 四值 **字符串**（`POSTED` / `EXISTS` / `SKIPPED` / `REVERSED_RERUN`），
 *    不是布尔：`EXISTS` 是幂等（本期已提过），`SKIPPED` 是本期无应提（合法空跑）；
 * 3. 红冲是**整批**红冲（红字凭证 + 明细 REVERSED + 卡片累计折旧回滚），原因后端要求 ≥ 2 字。
 */

import { get, post, type PageData } from '@/api/request';
import type { DepBatchStatus, DepRunOutcomeKind } from './asset-kinds';

export {
  DEP_BATCH_STATUSES,
  DEP_BATCH_STATUS_LABELS,
  DEP_BATCH_STATUS_THEMES,
  DEP_RUN_OUTCOMES,
  DEP_RUN_OUTCOME_LABELS,
} from './asset-kinds';

export type { DepBatchStatus, DepRunOutcomeKind } from './asset-kinds';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/** 丢掉空串 / null / undefined */
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

/** 折旧批次（逐字抄 `DepBatchRes`；金额字符串）。 */
export interface DepBatch {
  /** 主键 */
  id: string;
  /** 批次号 `DP-yyyyMM-0001` */
  batch_no: string;
  /** 期间 `yyyy-MM` */
  period_code: string;
  /** 状态 */
  status: DepBatchStatus | string;
  /** 资产台数 */
  asset_count: number;
  /** 合计金额（= 明细合计，巡检 K4） */
  total_amount: string;
  /** 计提凭证 */
  voucher_id: string | null;
  /** 红冲凭证 */
  reverse_voucher_id: string | null;
  /** 跳过原因（`SKIPPED` 时才有） */
  skip_reason: string | null;
  /** 运行时间 */
  run_at: string;
  /** 过账时间 */
  posted_at: string | null;
  /** 红冲时间 */
  reversed_at: string | null;
  /** 备注 */
  remark: string | null;
}

/** 折旧明细（逐字抄 `DepDetailRes`；卡片详情的 `depreciations` 也是它）。 */
export interface DepDetail {
  /** 主键 */
  id: string;
  /** 资产 id */
  asset_id: string;
  /** 资产编号 */
  asset_no: string;
  /** 资产名称 */
  asset_name: string;
  /** 期间 */
  period_code: string;
  /** 部门 id */
  dept_id: string;
  /** 部门名（快照） */
  dept_name: string;
  /** 费用科目编码（5602.01） */
  expense_account_code: string;
  /** 本月折旧 */
  amount: string;
  /** 计提前累计 */
  accumulated_before: string;
  /** 计提后累计 */
  accumulated_after: string;
  /** 计提后净值 */
  net_value_after: string;
  /** 是否尾差补齐期（最后一个月取差额，不取本位） */
  is_last_month: boolean;
  /** 明细状态（随批次） */
  status: string;
}

/** 批次详情（逐字抄 `DepBatchDetailRes`）。 */
export interface DepBatchDetail {
  /** 批次 */
  batch: DepBatch;
  /** 明细 */
  details: DepDetail[];
  /** 明细合计（必须等于 `batch.total_amount`） */
  detail_total: string;
  /** 可去的下一个状态 */
  next_statuses: string[];
}

/** 计提结果（逐字抄 `RunDepreciationRes`）。 */
export interface RunDepreciationResult {
  /** 结果类型：POSTED / EXISTS / SKIPPED / REVERSED_RERUN */
  outcome: DepRunOutcomeKind | string;
  /** 批次（`SKIPPED` 且期间不可写时可能为 null） */
  batch: DepBatch | null;
  /** 说明（后端给的中文，可直接展示） */
  message: string;
  /** 本次提了多少钱 */
  total_amount: string;
  /** 本次提了多少台 */
  asset_count: number;
}

/** 批次查询（`DepBatchFilter`）。 */
export interface DepBatchQuery {
  /** 页码 */
  page?: number;
  /** 每页条数（后端上限 100） */
  page_size?: number;
  /** 期间 `yyyy-MM` */
  period_code?: string;
  /** 状态 */
  status?: DepBatchStatus | '';
}

/** 手动计提入参（`RunDepreciationReq`）。 */
export interface RunDepreciationPayload {
  /** 目标期间 `yyyy-MM`（不传 = 上一个会计月，与定时任务同口径） */
  period_code?: string;
  /** 备注 */
  remark?: string;
}

/** 批次分页（`GET /depreciation/page`） */
export function fetchDepBatchPage(query: DepBatchQuery): Promise<PageData<DepBatch>> {
  return get<PageData<DepBatch>>(`${BASE}/depreciation/page`, asParams(query));
}

/** 批次详情（`GET /depreciation/{id}`，批次 + 明细 + 合计） */
export function fetchDepBatchDetail(id: string): Promise<DepBatchDetail> {
  return get<DepBatchDetail>(`${BASE}/depreciation/${id}`);
}

/** 手动计提（`POST /depreciation/run`） */
export function runDepreciation(payload: RunDepreciationPayload): Promise<RunDepreciationResult> {
  return post<RunDepreciationResult>(`${BASE}/depreciation/run`, payload);
}

/** 整批红冲（`POST /depreciation/{id}/reverse`，原因 ≥ 2 字；回批次详情） */
export function reverseDepreciation(id: string, reason: string): Promise<DepBatchDetail> {
  return post<DepBatchDetail>(`${BASE}/depreciation/${id}/reverse`, { reason });
}

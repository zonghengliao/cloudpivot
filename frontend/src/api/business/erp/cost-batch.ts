/**
 * 批次成本 API 层（M5-07 · 分批法：归集表 / 归集 / 结转 / 在制余额 / 台账）。
 *
 * 8 条接口，全部挂在 `/api/business/erp/cost-collect-sheet…` 下（后端注解不含 `/api`，
 * 该前缀由 `[router].prefix` 注入；前端必须写全 `/api` —— AGENTS 约定 9，
 * 漏了会命中 SPA fallback 拿到 HTML，报「响应体格式不符合」）：
 *
 * | # | 方法 | 路径 | 权限码 | 卡 |
 * |---|---|---|---|---|
 * | 1 | GET | `/cost-collect-sheet?period_code=yyyy-MM` | `cost-object:list` | M5-02 归集表 |
 * | 2 | GET | `/cost-collect-sheet/{object_id}?period_code=` | `cost-object:list` | M5-02 明细 |
 * | 3 | GET | `/cost-collect-sheet/{object_id}/sources?period_code=` | `cost-object:list` | M5-03 三路来源 |
 * | 4 | POST | `/cost-collect-sheet/{object_id}/collect` | `cost-period:calc` | M5-03 归集 |
 * | 5 | GET | `/cost-collect-sheet/{object_id}/finish-preview?period_code=` | `cost-object:list` | M5-04 结转试算 |
 * | 6 | POST | `/cost-collect-sheet/{object_id}/finish` | `cost-period:calc` | M5-04 结转 |
 * | 7 | GET | `/cost-collect-sheet/wip-balance?period_code=` | `cost-object:list` | M5-05 在制余额 |
 * | 8 | GET | `/cost-collect-sheet/{object_id}/wip-ledger?period_code=` | `cost-object:list` | M5-05 台账 |
 *
 * # 三条口径（后端逐字同源，页面不许弱化）
 *
 * 1. **`period_code` 必填**（没有默认本月）：归集额是「期间 × 批次」的量，
 *    服务端猜一个月份会让 1 月 1 日打开页面的人看到空表却以为数据丢了。
 *    页面用 `todayPeriod()` 做初值 + 记住上次选择；
 * 2. **写接口只接 `period_code`（结转多一个 `remark`）**：金额、行数、要素全部由
 *    来源单据推导 —— 这里没有「往批上填钱」的口子（`cost_batch_collect_dto` 模块头）；
 * 3. **列表 / 明细 / 余额的 404 是「看不见」不是「不存在」**：`{object_id}` 走数据范围探针，
 *    看不见就 404（不区分「已删」与「不在你范围」，避免把别人的批次号变成探测口）。
 */

import { get, post } from '@/api/request';

import type {
  BatchCollect,
  BatchFinish,
  BatchFinishPreview,
  BatchSources,
  CollectSheetQuery,
  SheetDetail,
  SheetPage,
  WipBalancePage,
  WipBalanceQuery,
  WipLedger,
} from './cost-batch-kinds';

export * from './cost-batch-kinds';

/** 接口前缀（后端注解不含 `/api`，约定 9 要求这里写全） */
const BASE = '/api/business/erp';

/** 丢掉空串 / null / undefined（`false` 也丢：两个开关的默认值都是「不限」） */
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

/** 路径参数编码（`object_id` 是 UUID，编码只为防御误传） */
function idPath(objectId: string): string {
  return encodeURIComponent(objectId.trim());
}

/** 1. 按批次归集表（分页 + 整张表合计；权限 `cost-object:list`） */
export function fetchCollectSheetPage(query: CollectSheetQuery): Promise<SheetPage> {
  return get<SheetPage>(`${BASE}/cost-collect-sheet`, asParams(query));
}

/** 2. 某批次某期间的逐笔归集明细（权限 `cost-object:list`） */
export function fetchCollectSheetDetail(objectId: string, periodCode: string): Promise<SheetDetail> {
  return get<SheetDetail>(`${BASE}/cost-collect-sheet/${idPath(objectId)}`, {
    period_code: periodCode,
  });
}

/** 3. 三路来源预览（料 / 工 / 费各哪几笔、哪些已归集、哪些不可归集；权限 `cost-object:list`） */
export function fetchBatchSources(objectId: string, periodCode: string): Promise<BatchSources> {
  return get<BatchSources>(`${BASE}/cost-collect-sheet/${idPath(objectId)}/sources`, {
    period_code: periodCode,
  });
}

/**
 * 4. 把本期该批的料 / 工 / 费归集进来（权限 `cost-period:calc`）。
 *
 * 入参**只有** `period_code`：金额由来源单据决定，与期间 `calc` 同一套幂等键，
 * 所以「先归集单批、再跑期间 calc」不会算两遍。
 */
export function collectBatch(objectId: string, periodCode: string): Promise<BatchCollect> {
  return post<BatchCollect>(`${BASE}/cost-collect-sheet/${idPath(objectId)}/collect`, {
    period_code: periodCode,
  });
}

/** 5. 完工结转试算（闸门 + 本次金额 + 历史；权限 `cost-object:list`） */
export function fetchBatchFinishPreview(
  objectId: string,
  periodCode: string,
): Promise<BatchFinishPreview> {
  return get<BatchFinishPreview>(`${BASE}/cost-collect-sheet/${idPath(objectId)}/finish-preview`, {
    period_code: periodCode,
  });
}

/** 6. 执行完工结转（权限 `cost-period:calc`；`inserted=false` 表示幂等空转） */
export function finishBatch(
  objectId: string,
  periodCode: string,
  remark = '',
): Promise<BatchFinish> {
  return post<BatchFinish>(`${BASE}/cost-collect-sheet/${idPath(objectId)}/finish`, {
    period_code: periodCode,
    remark: remark.trim() || null,
  });
}

/** 7. 在制批次余额（截止期间快照 + 整表合计；权限 `cost-object:list`） */
export function fetchWipBalance(query: WipBalanceQuery): Promise<WipBalancePage> {
  return get<WipBalancePage>(`${BASE}/cost-collect-sheet/wip-balance`, asParams(query));
}

/** 8. 在制台账（逐期「期初 / 投入 / 转出 / 期末」；权限 `cost-object:list`） */
export function fetchWipLedger(objectId: string, periodCode: string): Promise<WipLedger> {
  return get<WipLedger>(`${BASE}/cost-collect-sheet/${idPath(objectId)}/wip-ledger`, {
    period_code: periodCode,
  });
}

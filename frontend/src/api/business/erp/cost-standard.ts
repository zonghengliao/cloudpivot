/**
 * 标准成本卡 API 层（M5-09 · 后端 `cost_standard_{dto,handler,model,import}.rs`）。
 *
 * 10 个接口 / 4 个权限码（全部**复用** `cost-method:*`，零新增）：
 *
 * | 方法 | 路径 | 权限 |
 * |---|---|---|
 * | GET | `/api/business/erp/cost-standard` | `cost-method:list` |
 * | GET | `/api/business/erp/cost-standard/effective` | `cost-method:list` |
 * | GET | `/api/business/erp/cost-standard/period-sheet` | `cost-method:list` |
 * | GET | `/api/business/erp/cost-standard/import-template` | `cost-method:list` |
 * | GET | `/api/business/erp/cost-standard/{id}` | `cost-method:list` |
 * | POST | `/api/business/erp/cost-standard` | `cost-method:create` |
 * | POST | `/api/business/erp/cost-standard/import` | `cost-method:create` |
 * | PUT | `/api/business/erp/cost-standard/{id}` | `cost-method:update` |
 * | POST | `/api/business/erp/cost-standard/{id}/status` | `cost-method:update` |
 * | DELETE | `/api/business/erp/cost-standard/{id}` | `cost-method:delete` |
 *
 * # 三条后端口径（前端只负责把它们说清楚，不自己重算）
 *
 * 1. **同一物料启用版本的生效期不许重叠**（`409 + 66108`）：一条生效区间一个真相；
 * 2. **已生效过的版本**（`effective_from < 今天`）只能改「停止日 / 状态 / 备注」
 *    （`409 + 66109`），也不许删（`409 + 66110`）—— 它是那段期间口径的凭据；
 * 3. **导入是全有或全无**：任何一行有错，整批一条都不入库，逐行返回「行号 + 列名 + 原因」。
 */

import service, { del, get, post, put, type PageData } from '@/api/request';

import type {
  PeriodSheetQuery,
  PeriodSheetRes,
  StandardCreatePayload,
  StandardDetailRes,
  StandardEffectiveQuery,
  StandardEffectiveRes,
  StandardImportResult,
  StandardPageQuery,
  StandardRow,
  StandardStatusPayload,
  StandardStatusRes,
  StandardUpdatePayload,
} from './cost-standard-kinds';

export * from './cost-standard-kinds';

/** 接口前缀（后端注解不含 `/api`，约定 9 要求这里写全） */
const BASE = '/api/business/erp';

/** 丢掉空串 / null / undefined（`status` 的 0 要保留，所以判的是 `''` 而不是 falsy） */
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

/** 版本列表（`GET /business/erp/cost-standard`，权限 `cost-method:list`） */
export function fetchStandardPage(query: StandardPageQuery): Promise<PageData<StandardRow>> {
  return get<PageData<StandardRow>>(`${BASE}/cost-standard`, asParams(query));
}

/**
 * 某物料某期间（某天）生效的是哪一版（`GET …/cost-standard/effective`）。
 *
 * 出参带 `switch_in_period`（期间内换过版）与 `note`（差异分析按哪天取数）——
 * 差异分析页（M5-11…16）就靠它，维护页用它回答「这一期到底按哪版算」。
 */
export function fetchEffectiveStandard(query: StandardEffectiveQuery): Promise<StandardEffectiveRes> {
  return get<StandardEffectiveRes>(`${BASE}/cost-standard/effective`, asParams(query));
}

/** 期间 × 物料矩阵（`GET …/cost-standard/period-sheet`，期间必填） */
export function fetchPeriodSheet(query: PeriodSheetQuery): Promise<PageData<PeriodSheetRes>> {
  return get<PageData<PeriodSheetRes>>(`${BASE}/cost-standard/period-sheet`, asParams(query));
}

/** 版本详情（卡头 + 标准 BOM）。 */
export function fetchStandardDetail(id: string): Promise<StandardDetailRes> {
  return get<StandardDetailRes>(`${BASE}/cost-standard/${id}`);
}

/** 新建一版（`POST /business/erp/cost-standard`，权限 `cost-method:create`） */
export function createStandard(payload: StandardCreatePayload): Promise<StandardDetailRes> {
  return post<StandardDetailRes>(`${BASE}/cost-standard`, payload);
}

/** 改一版（`PUT /business/erp/cost-standard/{id}`，缺省的键 = 不改） */
export function updateStandard(id: string, payload: StandardUpdatePayload): Promise<StandardDetailRes> {
  return put<StandardDetailRes>(`${BASE}/cost-standard/${id}`, payload);
}

/**
 * 生效 / 失效（`POST …/cost-standard/{id}/status`）。
 *
 * 失效**不占生效窗口**：失效后同一期间可以按新口径重建一版（M5-08 的 V0463 专门放开了这条）。
 */
export function setStandardStatus(id: string, payload: StandardStatusPayload): Promise<StandardStatusRes> {
  return post<StandardStatusRes>(`${BASE}/cost-standard/${id}/status`, payload);
}

/** 软删一版（`DELETE …/cost-standard/{id}`，权限 `cost-method:delete`；已生效过 409/66110） */
export function deleteStandard(id: string): Promise<null> {
  return del<null>(`${BASE}/cost-standard/${id}`);
}

/**
 * 批量导入（`POST /business/erp/cost-standard/import`，multipart 字段 `file`）。
 *
 * **全有或全无**：返回 `success = 0` 时 `rows` 是逐行错误（一条都没入库）。
 */
export async function importStandards(file: File): Promise<StandardImportResult> {
  const form = new FormData();
  form.append('file', file);
  const response = await service.post<{ code: number; msg: string; data: StandardImportResult }>(
    `${BASE}/cost-standard/import`,
    form,
  );
  const body = response.data;
  if (!body || body.code !== 0) {
    throw new Error(body?.msg || '导入失败');
  }
  return body.data;
}

/** 下载 9 列导入模板（`GET …/cost-standard/import-template`，返回 xlsx 字节流） */
export async function downloadStandardTemplate(): Promise<Blob> {
  const response = await service.get<Blob>(`${BASE}/cost-standard/import-template`, {
    responseType: 'blob',
  });
  return response.data;
}

/** 触发浏览器保存（blob → 本地文件） */
export function saveBlob(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = filename;
  document.body.appendChild(link);
  link.click();
  document.body.removeChild(link);
  URL.revokeObjectURL(url);
}

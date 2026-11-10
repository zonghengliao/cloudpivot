/**
 * 工时台账 API 层（ARCH-9 §5.2 #10–#16，后端 `cost_labor_{dto,handler,model,import}.rs`）。
 *
 * 词表与契约类型在 `cost-labor-kinds.ts`（不 import 请求层，纯逻辑单测可引用），
 * 本文件原样 `export *` 出来 —— 页面仍然只认 `@/api/business/erp/cost-labor` 一个入口。
 *
 * # 四个容易踩的点
 *
 * 1. **路径写全 `/api`**（后端注解不含前缀，漏了会命中 SPA fallback 返回 HTML，
 *    报「响应体格式不符合」）；
 * 2. **金额 / 工时 / 费率一律字符串**（numeric 出 JSON 是 `"400.0000"`），类型上就声明 `string`；
 * 3. **导入是 multipart + 全有或全无**：`file`（xlsx）必填、`period_code` 可选；
 *    任何一行有错 ⇒ `{success:0, failed:n, rows:[…]}`，**一条都不入库**；
 * 4. **模板下载返回二进制**（不走 `Res<T>` 信封），必须 `responseType: 'blob'` 并自行触发保存。
 */

import service, { del, get, post, put, type PageData } from '@/api/request';

import type {
  CostLaborCreatePayload,
  CostLaborImportResult,
  CostLaborPageQuery,
  CostLaborRow,
  CostLaborUpdatePayload,
} from './cost-labor-kinds';

export * from './cost-labor-kinds';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/** 丢掉空串 / null / undefined（后端把 `?keyword=` 当「等于空串」过滤，会查出 0 行） */
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

/** 列表分页（`GET /business/erp/cost-labor`，权限 `cost-labor:list`） */
export function fetchCostLaborPage(query: CostLaborPageQuery): Promise<PageData<CostLaborRow>> {
  return get<PageData<CostLaborRow>>(`${BASE}/cost-labor`, asParams(query));
}

/**
 * 新建工时（`POST /business/erp/cost-labor`，权限 `cost-labor:create`）。
 *
 * `rate` 留空 ⇒ 后端自动取数（对象 > 车间 > 公司）；三级全空 ⇒ `422 + 66059`。
 * 回单里的 `rate_scope` / `rate_scope_label` 就是「实际取到哪一级」——页面显示在费率右侧。
 */
export function createCostLabor(payload: CostLaborCreatePayload): Promise<CostLaborRow> {
  return post<CostLaborRow>(`${BASE}/cost-labor`, payload);
}

/** 编辑工时（`PUT /business/erp/cost-labor/{id}`，权限 `cost-labor:update`；仅 `DRAFT`） */
export function updateCostLabor(id: string, payload: CostLaborUpdatePayload): Promise<CostLaborRow> {
  return put<CostLaborRow>(`${BASE}/cost-labor/${id}`, payload);
}

/** 删除工时（`DELETE /business/erp/cost-labor/{id}`，权限 `cost-labor:delete`；软删，仅 `DRAFT`） */
export function deleteCostLabor(id: string): Promise<null> {
  return del<null>(`${BASE}/cost-labor/${id}`);
}

/**
 * 确认工时（`POST /business/erp/cost-labor/{id}/confirm`，权限 `cost-labor:update`）。
 *
 * **幂等**：已 `CONFIRMED` 再确认仍返回成功且不写库（`updated_at` 都不动）。
 */
export function confirmCostLabor(id: string): Promise<CostLaborRow> {
  return post<CostLaborRow>(`${BASE}/cost-labor/${id}/confirm`);
}

/**
 * 导入工时（`POST /business/erp/cost-labor/import`，权限 `cost-labor:import`）。
 *
 * `periodCode` 给了 ⇒ 逐行核对日期是否落在该期间；不给 ⇒ 按每行日期自动归属期间。
 */
export async function importCostLabors(
  file: File,
  periodCode?: string,
): Promise<CostLaborImportResult> {
  const form = new FormData();
  form.append('file', file);
  if (periodCode) {
    form.append('period_code', periodCode);
  }
  const response = await service.post<{ code: number; msg: string; data: CostLaborImportResult }>(
    `${BASE}/cost-labor/import`,
    form,
  );
  const body = response.data;
  if (!body || body.code !== 0) {
    throw new Error(body?.msg || '导入失败');
  }
  return body.data;
}

/** 下载 8 列导入模板（`GET /business/erp/cost-labor/template`，返回 xlsx 字节流） */
export async function downloadCostLaborTemplate(): Promise<Blob> {
  const response = await service.get<Blob>(`${BASE}/cost-labor/template`, {
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

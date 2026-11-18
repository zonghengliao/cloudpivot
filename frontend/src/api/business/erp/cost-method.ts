/**
 * 成本方法配置 API 层（M5-01 · 后端 `cost_method_{dto,handler,model}.rs`）。
 *
 * 五个接口（4 个权限码：`list` 同时承载两条只读接口）：
 *
 * | 方法 | 路径 | 权限 |
 * |---|---|---|
 * | GET | `/api/business/erp/cost-method` | `cost-method:list` |
 * | GET | `/api/business/erp/cost-method/effective` | `cost-method:list` |
 * | POST | `/api/business/erp/cost-method` | `cost-method:create` |
 * | PUT | `/api/business/erp/cost-method/{id}` | `cost-method:update` |
 * | DELETE | `/api/business/erp/cost-method/{id}` | `cost-method:delete` |
 *
 * # 三条口径（与后端逐字对应）
 *
 * 1. 同范围同生效日只有一条版本；区间重叠后端 `409 + 66102`；
 * 2. 已生效过（`effective_from < 今天`）的版本只允许改 停止日 / 状态 / 备注（`409 + 66105`），
 *    也不允许删除（`409 + 66104`）；
 * 3. 本期生效范围里有未实现方法（`VARIETY` / `STANDARD`）⇒ 成本计算入口 `409 + 66101`。
 */

import { del, get, post, put, type PageData } from '@/api/request';

import type {
  CostMethodCreatePayload,
  CostMethodEffectiveQuery,
  CostMethodEffectiveRes,
  CostMethodPageQuery,
  CostMethodRow,
  CostMethodUpdatePayload,
} from './cost-method-kinds';

export * from './cost-method-kinds';

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

/** 列表分页（`GET /business/erp/cost-method`，权限 `cost-method:list`） */
export function fetchCostMethodPage(
  query: CostMethodPageQuery,
): Promise<PageData<CostMethodRow>> {
  return get<PageData<CostMethodRow>>(`${BASE}/cost-method`, asParams(query));
}

/**
 * 查生效方法（`GET /business/erp/cost-method/effective`，权限 `cost-method:list`）。
 *
 * 出参带**来源**（对象级 / 全局 / 兜底）与**能否计算**（`method_implemented`），
 * 页面据此在用户点「计算成本」之前就说清「这一期算不出来」。
 */
export function fetchEffectiveMethod(
  query: CostMethodEffectiveQuery,
): Promise<CostMethodEffectiveRes> {
  return get<CostMethodEffectiveRes>(`${BASE}/cost-method/effective`, asParams(query));
}

/** 新建配置（`POST /business/erp/cost-method`，权限 `cost-method:create`） */
export function createCostMethod(payload: CostMethodCreatePayload): Promise<CostMethodRow> {
  return post<CostMethodRow>(`${BASE}/cost-method`, payload);
}

/** 修改配置（`PUT /business/erp/cost-method/{id}`，权限 `cost-method:update`） */
export function updateCostMethod(
  id: string,
  payload: CostMethodUpdatePayload,
): Promise<CostMethodRow> {
  return put<CostMethodRow>(`${BASE}/cost-method/${id}`, payload);
}

/** 删除配置（`DELETE /business/erp/cost-method/{id}`，权限 `cost-method:delete`；软删） */
export function deleteCostMethod(id: string): Promise<null> {
  return del<null>(`${BASE}/cost-method/${id}`);
}

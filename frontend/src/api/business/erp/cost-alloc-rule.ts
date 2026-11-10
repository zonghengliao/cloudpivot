/**
 * 分摊规则 API 层（ARCH-9 §5.2 #20–#24，后端 `cost_alloc_rule_{dto,handler,model}.rs`）。
 *
 * 五个接口：分页 / 新建 / 修改 / 删除（软删）/ 试算（**只算不写**）。
 *
 * # 口径
 *
 * | 事情 | 后端 | 页面 |
 * |------|------|------|
 * | 试算守恒 | `Σ alloc_amount == pool_amount`（最大余数法，尾差只落一行） | 表尾打勾（`decimalEquals`） |
 * | 试算硬错误 | 池>0 无在产对象 `409 + 66062`；基数合计 0 `409 + 66063` | 原样展示 `msg`（已点名车间/科目/金额） |
 * | 修改 | 已参与分摊的版本只放行 名称/备注/状态，其余 `409` | 409 报文引导"新建版本" |
 * | 删除 | 已被分摊结果引用 ⇒ `409`（点名期间） | 原样展示 |
 */

import { del, get, post, put, type PageData } from '@/api/request';

import type {
  CostAllocPreviewReq,
  CostAllocPreviewRes,
  CostAllocRuleCreatePayload,
  CostAllocRulePageQuery,
  CostAllocRuleRow,
  CostAllocRuleUpdatePayload,
} from './cost-alloc-rule-kinds';

export * from './cost-alloc-rule-kinds';

/** 接口前缀（后端注解不含 `/api`） */
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

/** 列表分页（`GET /business/erp/cost-alloc-rule`，权限 `cost-alloc-rule:list`） */
export function fetchCostAllocRulePage(
  query: CostAllocRulePageQuery,
): Promise<PageData<CostAllocRuleRow>> {
  return get<PageData<CostAllocRuleRow>>(`${BASE}/cost-alloc-rule`, asParams(query));
}

/** 新建规则（`POST /business/erp/cost-alloc-rule`，权限 `cost-alloc-rule:create`） */
export function createCostAllocRule(
  payload: CostAllocRuleCreatePayload,
): Promise<CostAllocRuleRow> {
  return post<CostAllocRuleRow>(`${BASE}/cost-alloc-rule`, payload);
}

/** 修改规则（`PUT /business/erp/cost-alloc-rule/{id}`，权限 `cost-alloc-rule:update`） */
export function updateCostAllocRule(
  id: string,
  payload: CostAllocRuleUpdatePayload,
): Promise<CostAllocRuleRow> {
  return put<CostAllocRuleRow>(`${BASE}/cost-alloc-rule/${id}`, payload);
}

/** 删除规则（`DELETE /business/erp/cost-alloc-rule/{id}`，权限 `cost-alloc-rule:delete`；软删） */
export function deleteCostAllocRule(id: string): Promise<null> {
  return del<null>(`${BASE}/cost-alloc-rule/${id}`);
}

/**
 * 试算（`POST /business/erp/cost-alloc-rule/preview`，权限 `cost-alloc-rule:preview`）。
 *
 * **只算不写**：不产生 `biz_cost_alloc_result`，可以反复点。
 * 池里有钱但当期没在产对象 / 该标准基数合计为 0 ⇒ `409`（`66062` / `66063`），
 * 报文里点名车间、科目、金额 —— 这两条是硬错误，不做"留在池里下期再说"的静默处理。
 */
export function previewCostAllocation(req: CostAllocPreviewReq): Promise<CostAllocPreviewRes> {
  return post<CostAllocPreviewRes>(`${BASE}/cost-alloc-rule/preview`, req);
}

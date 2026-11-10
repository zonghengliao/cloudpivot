/**
 * 手工费用台账 API 层（ARCH-9 §5.2 #17–#19，后端 `cost_overhead_{dto,handler,model}.rs`）。
 *
 * 三个接口：分页 / 新增 / 删除（软删）。**没有编辑**（设计只给了 3 个），
 * 也**没有批量**（不是"漏了"）。
 *
 * # 口径
 *
 * | 事情 | 后端 | 页面 |
 * |------|------|------|
 * | 金额 | 字符串（`"3000.0000"`） | 类型上声明 `string`，展示走 `shared/money.ts` |
 * | 删除 | 已被 `biz_cost_collect` 引用 ⇒ `409`（点名期间） | 原样展示 `msg`（已经带"怎么修"） |
 * | 科目 | `4101.*` + 存在 + 末级 + 启用 | 提交前本地先拦前缀，库层校验仍以后端为准 |
 */

import { del, get, post, type PageData } from '@/api/request';

import type {
  CostOverheadCreatePayload,
  CostOverheadPageQuery,
  CostOverheadRow,
} from './cost-overhead-kinds';

export * from './cost-overhead-kinds';

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

/** 列表分页（`GET /business/erp/cost-overhead`，权限 `cost-overhead:list`） */
export function fetchCostOverheadPage(
  query: CostOverheadPageQuery,
): Promise<PageData<CostOverheadRow>> {
  return get<PageData<CostOverheadRow>>(`${BASE}/cost-overhead`, asParams(query));
}

/** 新增费用（`POST /business/erp/cost-overhead`，权限 `cost-overhead:create`） */
export function createCostOverhead(payload: CostOverheadCreatePayload): Promise<CostOverheadRow> {
  return post<CostOverheadRow>(`${BASE}/cost-overhead`, payload);
}

/**
 * 删除费用（`DELETE /business/erp/cost-overhead/{id}`，权限 `cost-overhead:delete`）。
 *
 * 软删。已被成本归集引用 ⇒ `409`（报文点名期间）——那说明这条费用已经进了当期成本，
 * 删了会让账对不上，正确做法是作废来源单据 / 红冲期间。
 */
export function deleteCostOverhead(id: string): Promise<null> {
  return del<null>(`${BASE}/cost-overhead/${id}`);
}

/**
 * 「未归集清单」API 层（COST-B2-12 / ARCH-9 §5.2 #33 · §6.2.2 ②）。
 *
 * 词表与契约类型在 `cost-uncollected-kinds.ts`（不 import 请求层，纯逻辑单测可引用），
 * 本文件原样 `export *` 出来 —— 页面只认 `@/api/business/erp/cost-uncollected` 一个入口。
 *
 * # 两条路径的关系（**只能选一条**，不要两条都调）
 *
 * | 场景 | 用哪个 | 为什么 |
 * |---|---|---|
 * | 向导第 ② 步「自查缺口」 | [`fetchUncollectedByPeriod`] | 这时批次**还没建**（第 ③ 步 `calc` 才建），没有 id 可传 |
 * | 批次已存在，按批次查 | [`fetchUncollectedByBatch`] | 设计 §5.2 #33 的原文路径 |
 *
 * 两者出参结构完全一致（后端同一个装配函数），所以页面拿 `period_code` 一个入参就够，
 * 切批次时不需要换渲染逻辑。
 *
 * # 路径必须写全 `/api`
 *
 * 后端注解不含前缀（`[router].prefix` 注入）；漏了会命中 SPA fallback 返回 HTML，
 * 报「响应体格式不符合」——这是本仓最高频的前端踩坑。
 */

import { get } from '@/api/request';

import type { CostUncollectedRes } from './cost-uncollected-kinds';

export * from './cost-uncollected-kinds';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/**
 * 按期间自查缺口（`GET /business/erp/cost-period/uncollected?period_code=yyyy-MM`）。
 *
 * 权限 `business:erp:cost-period:list`。永远 200（设计：不阻断），缺口在 `gap_count` 里。
 */
export function fetchUncollectedByPeriod(periodCode: string): Promise<CostUncollectedRes> {
  return get<CostUncollectedRes>(`${BASE}/cost-period/uncollected`, {
    period_code: periodCode.trim(),
  });
}

/**
 * 按批次自查缺口（`GET /business/erp/cost-period/{id}/uncollected`，设计 §5.2 #33）。
 *
 * 批次不存在 ⇒ `404 + 40004`（`RESOURCE_NOT_FOUND`），文案里带按期间查的入口。
 */
export function fetchUncollectedByBatch(batchId: string): Promise<CostUncollectedRes> {
  return get<CostUncollectedRes>(`${BASE}/cost-period/${batchId}/uncollected`);
}

/**
 * 差异分析 API 层（M5-19 · 后端 `cost_standard_compare_{dto,handler,model}.rs`，M5-11 交付）。
 *
 * 一条只读报表：
 *
 * | 方法 | 路径 | 权限 |
 * |---|---|---|
 * | GET | `/api/business/erp/report/cost-variance` | `business:erp:cost-method:list` |
 *
 * # 为什么只有一个接口
 *
 * 设计 §5.2 #36 只给了这一条（原口径「期间 × 成本对象 × 差异类型」的差异报表）。M5-11 交的是
 * 它的**汇总层**：一个物料一行，出「标准 / 实际 / 差异额 / 差异率 / 三要素拆分」+ 整表合计。
 * 钻取（某物料某批的六类差异明细）要等 `biz_cost_variance` 落表后另开子路径，
 * **不改本路径的出参结构** —— 所以本页不做「点开某行再拉一次」的钻取，
 * 行内展开用的是同一条响应里已经带回来的 `variance.elements`（不多打一个请求，也不会两处口径打架）。
 *
 * # 期间必填
 *
 * 后端 `filter_of` 对缺 / 空 / 非法期间一律 400（不默认「上个月」：报表跨期猜错月份没人看得出）。
 * 页面在期间为空时**不发请求**（返回空页），见 `cost-variance-logic.ts::buildQuery`。
 */

import { get } from '@/api/request';

import type { CostVarianceQuery, CostVarianceRes } from './cost-variance-kinds';

export * from './cost-variance-kinds';

/** 接口前缀（后端注解不含 `/api`，约定 9 要求这里写全） */
const BASE = '/api/business/erp';

/** 丢掉空串 / `null` / `undefined`（`page=1` 要保留，所以判的是 `''` 而不是 falsy） */
function asParams(query: CostVarianceQuery): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === '') {
      continue;
    }
    out[key] = value;
  }
  return out;
}

/**
 * 逐物料差异报表（`GET /business/erp/report/cost-variance`）。
 *
 * 出参 `list` / `total` / `page` / `page_size` 与 `PageData` 同构，另带 `totals` / `note` /
 * `batch` / `cost_ready` —— 页面把整份响应存下来给汇总卡片与口径说明用
 * （合计是后端按**筛选后全集**算的，前端不累加：服务端分页下前端手里只有一页）。
 */
export function fetchCostVariance(query: CostVarianceQuery): Promise<CostVarianceRes> {
  return get<CostVarianceRes>(`${BASE}/report/cost-variance`, asParams(query));
}

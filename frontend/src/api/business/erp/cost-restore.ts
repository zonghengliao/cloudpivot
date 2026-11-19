/**
 * 成本还原 API 层（M5-30 · 消费 M5-27 还原明细 + M5-28 还原后报表）。
 *
 * | 方法 | 路径 | 权限 | 说明 |
 * |---|---|---|---|
 * | GET | `/api/business/erp/cost-restore-rollup` | `business:erp:cost-object:list` | 逐成本对象还原：还原前料工费 → 逐层卷积 → 还原后料工费（+ 逐层台账 + 半成品明细） |
 * | GET | `/api/business/erp/cost-restore-report` | `business:erp:cost-object:list` | 按车间 / 产品 / 对象 / 整表分组的料工费占比与结构变化 |
 *
 * 词表与契约类型在 `cost-restore-kinds.ts`（那里不 import 请求层，纯逻辑单测可直接引用），
 * 本文件原样 `export *` 出来 —— 页面仍只认 `@/api/business/erp/cost-restore` 一个入口。
 *
 * # 三个容易踩的点
 *
 * 1. **路径写全 `/api`**（后端注解不含前缀，AGENTS 约定 9）。漏了会命中 SPA fallback 返回 HTML，
 *    报「响应体格式不符合」而不是 404；
 * 2. **期间必填**：缺 / 空 / 格式不对，后端一律 400（不默认「上个月」—— 报表跨期猜错月份
 *    没人看得出）。页面在期间为空时**不发请求**，见 `cost-restore-logic::buildRollupQuery`
 *    返回 `null` 的那条分支；
 * 3. **两个接口都是只读**：没有任何写动作，也没有「事务提交后失败留死单」的面（AGENTS 约定 11）。
 *    页面因此不挂保存按钮、不做整表覆盖提交。
 */

import { get } from '@/api/request';

import type { RestoreReportQuery, RestoreReportRes, RestoreRollupQuery, RestoreRollupRes } from './cost-restore-kinds';

export * from './cost-restore-kinds';

/** 接口前缀（后端注解不含 `/api`，AGENTS 约定 9 要求这里写全） */
const BASE = '/api/business/erp';

/** 丢掉空串 / `null` / `undefined`（`page=1` 要保留，所以判的是 `''` 而不是 falsy） */
function asParams(query: object): Record<string, unknown> {
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
 * 还原明细表（`GET /business/erp/cost-restore-rollup`）。
 *
 * 出参 `list / total / page / page_size` 与 `PageData` 同构（`ListPage` 直接吃），
 * 另带 `totals` / `formula_restore` / `formula_unresolved` / `note` / `batch` / `cost_ready` /
 * `state_options` —— 页面把整份响应存下来给汇总卡片与口径说明用。
 *
 * **合计用后端 `totals`，前端不累加**：表是服务端分页的，前端手里只有当前页，
 * 拿当前页加出来的「还原前合计」会随翻页变，而财务会拿它跟成本对象台账对。
 */
export function fetchRestoreRollup(query: RestoreRollupQuery): Promise<RestoreRollupRes> {
  return get<RestoreRollupRes>(`${BASE}/cost-restore-rollup`, asParams(query));
}

/**
 * 还原后报表（`GET /business/erp/cost-restore-report`）。
 *
 * 出参 `groups / total / page / page_size` 是**分组分页**（一页 50 组），`totals` 是整表合计。
 * 占比、占比变化（pp）、`share_sum`、`dominant` 全部由后端算好 —— 页面只负责显示，
 * 而且**不硬判 `share_sum === 100`**（见 `cost-restore-kinds::SHARE_SUM_TOLERANCE`）。
 */
export function fetchRestoreReport(query: RestoreReportQuery): Promise<RestoreReportRes> {
  return get<RestoreReportRes>(`${BASE}/cost-restore-report`, asParams(query));
}

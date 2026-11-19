/**
 * 在制品 API 层（M5-25 · 消费 M5-21 工序完工率 + M5-23 期末在制余额）。
 *
 * | 方法 | 路径 | 权限 | 说明 |
 * |---|---|---|---|
 * | GET | `/api/business/erp/cost-object/{id}/operations` | `business:erp:cost-object:list` | 该批的工序完工率 + 加权汇总 + 录入闸 |
 * | PUT | `/api/business/erp/cost-object/{id}/operations` | `business:erp:cost-object:update` | **整表覆盖**保存（缺的 `seq` 软删）+ 同事务回写对象级完工率 |
 * | GET | `/api/business/erp/cost-wip-balance` | `business:erp:cost-object:list` | 期末在制余额（逐成本对象：期初 / 投入 / 完工转出 / 期末 + 结转核对） |
 *
 * 词表与契约类型在 `cost-wip-kinds.ts`（那里不 import 请求层，纯逻辑单测可直接引用），
 * 本文件原样 `export *` 出来 —— 页面仍只认 `@/api/business/erp/cost-wip` 一个入口。
 *
 * # 三个容易踩的点
 *
 * 1. **路径写全 `/api`**（后端注解不含前缀）。漏了会命中 SPA fallback 返回 HTML，
 *    报「响应体格式不符合」而不是 404；
 * 2. **`PUT` 是整表覆盖，不是增量**：页面上删掉一行 ⇒ 保存后库里那行软删。
 *    反过来，如果把某行「暂时没填」当成不提交，那行的工序就没了 —— 所以
 *    `cost-wip-logic::toSavePayload` 把表格里**看得见的每一行**都提交；
 * 3. **期间必填**（余额接口）：缺 / 空 / 格式不对，后端一律 400（不默认「上个月」：
 *    报表跨期猜错月份没人看得出）。页面在期间为空时**不发请求**，见
 *    `cost-wip-logic::buildBalanceQuery` 返回 `null`。
 */

import { get, put } from '@/api/request';

import type {
  WipBalanceQuery,
  WipBalanceRes,
  WipOperationRes,
  WipOperationSaveReq,
} from './cost-wip-kinds';

export * from './cost-wip-kinds';

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

/** 路径参数兜底：空 id 直接抛，别让请求打到 `/cost-object//operations`（会命中别的路由） */
function idPath(id: string): string {
  const text = (id ?? '').trim();
  if (!text) {
    throw new Error('成本对象 id 不能为空');
  }
  return encodeURIComponent(text);
}

/**
 * 读该批的工序完工率（`GET /business/erp/cost-object/{id}/operations`）。
 *
 * 出参里同时带三样东西，页面一次拿全、不多打请求：
 * 工序行（`rows`）、加权汇总（`rollup`，没有工序时 `null`）、录入闸（`state` /
 * `object.blocked_reason`）。`rollup.pct` 就是写回对象级的那个数 —— 页面显示它，
 * **不要**用 `rows` 自己再算一遍（`cost_wip_pct_dto::rollup_of` 是唯一的加权实现）。
 */
export function fetchWipOperations(objectId: string): Promise<WipOperationRes> {
  return get<WipOperationRes>(`${BASE}/cost-object/${idPath(objectId)}/operations`);
}

/**
 * 整表保存工序完工率（`PUT /business/erp/cost-object/{id}/operations`）。
 *
 * 同事务里做三件事：按 `seq` 对齐增 / 改 / 软删 → 加权汇总 → 回写
 * `biz_cost_object.completion_pct`。出参与 GET 同构（多带 `changed / inserted /
 * updated / deleted` 四个计数），页面用它刷新表格、汇总与提醒。
 *
 * 409 = 业务状态不允许（已关批 / 已作废 / 该批参与的期间已结算）——
 * 报文是后端写好的中文，页面原样展示并附上「去哪修」的指引（见 `saveErrorText`）。
 */
export function saveWipOperations(
  objectId: string,
  payload: WipOperationSaveReq,
): Promise<WipOperationRes> {
  return put<WipOperationRes>(`${BASE}/cost-object/${idPath(objectId)}/operations`, payload);
}

/**
 * 期末在制余额表（`GET /business/erp/cost-wip-balance`）。
 *
 * 出参 `list / total / page / page_size` 与 `PageData` 同构（`ListPage` 直接吃），
 * 另带 `totals` / `formula_roll` / `formula_carry` / `note` / `batch` / `reversed_batch` /
 * `cost_ready` / `state_options` —— 页面把整份响应存下来给汇总卡片与口径说明用。
 *
 * **合计用后端 `totals`，前端不累加**：表是服务端分页的，前端手里只有当前页，
 * 拿当前页加出来的「期末余额合计」会随翻页变，而财务会拿它跟总账对。
 */
export function fetchWipBalance(query: WipBalanceQuery): Promise<WipBalanceRes> {
  return get<WipBalanceRes>(`${BASE}/cost-wip-balance`, asParams(query));
}

/**
 * 分页出参的别名。
 *
 * `WipBalanceRes` 本身就带 `list / total / page / page_size`（与 `PageData` 同构），
 * 所以页面 `fetcher` 直接返回整份响应即可，不需要再包一层。
 */
export type WipBalancePage = WipBalanceRes;


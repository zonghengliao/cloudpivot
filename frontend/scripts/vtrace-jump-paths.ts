/**
 * VTRACE-6：凭证来源 → 前端跳转路径的**真跑**校验器（e2e 用，不是单测）。
 *
 * 被 `scripts/verify/vtrace_e2e.sh` 在最后一步调用：
 *
 * ```bash
 * cd frontend && pnpm exec tsx scripts/vtrace-jump-paths.ts <cases.json>
 * ```
 *
 * 输入（由 shell 脚本写出的 JSON）：
 *
 * ```json
 * {
 *   "require_mapping": ["STOCK_IN", "STOCK_OUT", "EXPENSE_REPORT", "DEPRECIATION", "COST_PERIOD", "MANUAL"],
 *   "cases": [
 *     { "key": "PURCHASE", "source_type": "STOCK_IN", "source_id": "<uuid>", "source_no": "IN-…",
 *       "resolvable": true, "expect_path": "/business/erp/stock-in", "expect_query": {"in_id": "<uuid>"} }
 *   ]
 * }
 * ```
 *
 * 断言：
 * 1. `require_mapping` 里每个类型都在 `SOURCE_ROUTE_TABLE` 里有一行（缺 → 红，提醒补映射表）；
 * 2. 每条 case：`resolveSourceRoute()` 的返回值（null 还是链接）与 `resolvable` 一致；
 * 3. 可跳的：`path` 必须**已注册**在 `router/index.ts`（本仓路由手工注册，漏了 = 404），
 *    且 query 逐键相等（`deepLink='none'` 的行必须给空 query —— 不塞假参数）；
 * 4. 不可跳的：给出人能看懂的原因（MANUAL / 页面未落地 / 参数缺失），并打印表里的 note。
 *
 * 为什么不用 node:test：这是「跑一次真实例后的对账」，输入是活的接口数据（真 uuid、
 * 真单号），不是固定夹具；用测试跑会把「今天这批单据」写死进仓库。
 */
import { readFileSync } from 'node:fs';

import {
  SOURCE_ROUTE_TABLE,
  resolveSourceRoute,
  isSourceDocType,
} from '../src/views/business/erp/shared/source-route';

interface Case {
  key: string;
  source_type: string;
  source_id?: string | null;
  source_no?: string | null;
  resolvable: boolean;
  expect_path?: string;
  expect_query?: Record<string, string>;
  expect_deep_link?: string;
  note?: string;
}

interface Cases {
  require_mapping: string[];
  cases: Case[];
}

const file = process.argv[2];
if (!file) {
  console.error('用法：tsx scripts/vtrace-jump-paths.ts <cases.json>');
  process.exit(2);
}

const payload = JSON.parse(readFileSync(file, 'utf8')) as Cases;

/** 路由注册表：从 `router/index.ts` 源文本里抠（与 vtrace-source-route 单测同口径）。 */
const routerSrc = readFileSync(new URL('../src/router/index.ts', import.meta.url), 'utf8');
const registered = new Set(
  [...routerSrc.matchAll(/path:\s*'([^']+)'/g)].map((m) => `/${m[1].replace(/^\/+/, '')}`),
);

let pass = 0;
let fail = 0;

function ok(message: string): void {
  pass += 1;
  console.log(`[PASS] ${message}`);
}

function bad(message: string): void {
  fail += 1;
  console.log(`[FAIL] ${message}`);
}

function sameQuery(actual: Record<string, string>, expected: Record<string, string>): boolean {
  const a = Object.keys(actual).sort();
  const b = Object.keys(expected).sort();
  if (a.length !== b.length) {
    return false;
  }
  return a.every((key, index) => key === b[index] && actual[key] === expected[key]);
}

console.log('== ① 映射表覆盖（卡面 6 种 → 真实 source_type 都必须在表里）');
for (const type of payload.require_mapping) {
  if (isSourceDocType(type)) {
    const spec = SOURCE_ROUTE_TABLE[type as keyof typeof SOURCE_ROUTE_TABLE];
    ok(`${type} 有映射（${spec.label}，deepLink=${spec.deepLink}）`);
  } else {
    bad(`${type} 不在 SOURCE_ROUTE_TABLE —— 缺映射，请补进 frontend/src/views/business/erp/shared/source-route.ts`);
  }
}

console.log('');
console.log('== ② 逐条解析（真单据 id / 真单号 → 跳转路径）');
for (const item of payload.cases) {
  const route = resolveSourceRoute(item.source_type, item.source_id, item.source_no);
  const label = `${item.key}(${item.source_type})`;

  if (!item.resolvable) {
    if (route !== null) {
      bad(`${label} 期望不可跳（null），实际给了 ${route.path}?${JSON.stringify(route.query)}`);
      continue;
    }
    const spec = isSourceDocType(item.source_type)
      ? SOURCE_ROUTE_TABLE[item.source_type as keyof typeof SOURCE_ROUTE_TABLE]
      : null;
    const reason = item.note ?? spec?.note ?? '未识别来源';
    ok(`${label} 按预期不可跳：${reason}`);
    continue;
  }

  if (route === null) {
    bad(`${label} 期望可跳，实际返回 null（表里 note：${item.note ?? '—'}）`);
    continue;
  }
  if (route.path !== item.expect_path) {
    bad(`${label} path 期望 [${item.expect_path}] 实际 [${route.path}]`);
    continue;
  }
  if (!registered.has(route.path)) {
    bad(`${label} 目标页 ${route.path} 未在 router/index.ts 注册（点进去 404）`);
    continue;
  }
  const expectedQuery = item.expect_query ?? {};
  if (!sameQuery(route.query, expectedQuery)) {
    bad(`${label} query 期望 ${JSON.stringify(expectedQuery)} 实际 ${JSON.stringify(route.query)}`);
    continue;
  }
  if (item.expect_deep_link && route.deepLink !== item.expect_deep_link) {
    bad(`${label} deepLink 期望 [${item.expect_deep_link}] 实际 [${route.deepLink}]`);
    continue;
  }
  const qs = Object.entries(route.query)
    .map(([k, v]) => `${k}=${v}`)
    .join('&');
  ok(`${label} → ${route.path}${qs ? `?${qs}` : ''}（${route.deepLink}，${route.label}）`);
}

console.log('');
console.log(`== 跳转校验汇总：PASS=${pass} FAIL=${fail}`);
process.exit(fail === 0 ? 0 : 1);

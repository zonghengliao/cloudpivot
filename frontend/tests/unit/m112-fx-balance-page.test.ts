/**
 * M1-12：外币余额表**页面**契约单测（页面本体由 M1-07 落地，本卡补真机验收 + 契约钉死）。
 *
 * 运行：`cd frontend && pnpm test:unit`（`tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 为什么还要一份「源码即契约」测
 *
 * `tests/unit/erp-fx-balance.test.ts`（M1-07）钉的是**纯逻辑**：金额格式化、借贷方向、
 * 缺牌价占位符。但真正会静默把报表画错的是**模板里调用谁**：
 *
 * - 折算列若从 `amountCell(row.converted_base)` 换成 `formatAmount(row.converted_base)`，
 *   缺牌价的格子会从「—」变成**空串**；再有人补个 `|| '0.00'` 就变成「折算是 0 元」——
 *   财务会直接抄走这个 0。逻辑函数没变，逻辑单测全绿，报表却错了。
 * - KPI「折算合计」若从 `convertedTotalText(report)` 换成 `formatAmount(...)`，
 *   半套折算的 `null` 合计会被画成空串/0。
 * - 币种码 / 路径 / `meta.permission` 与迁移 `V0412` 漂了，用户点菜单直接 404 或 403。
 *
 * 所以这里断言的是**页面源码的调用形态**（与 `m111-voucher-fx-columns.test.ts` 同一套做法）：
 * 只读源码做子串 / 正则断言，不引 DOM、不引浏览器。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

import {
  MISSING_RATE_PLACEHOLDER,
  PERM_FX_BALANCE,
  amountCell,
  formatAmount,
  sideOf,
} from '@/views/business/erp/report/fx-balance/fx-balance-logic';

const ROOT = resolve(process.cwd());

/** 读一个文件（SFC / ts / 路由表），只做子串与正则断言 */
function sourceOf(relative: string): string {
  return readFileSync(join(ROOT, relative), 'utf8');
}

const PAGE = sourceOf('src/views/business/erp/report/fx-balance/index.vue');
const LOGIC = sourceOf('src/views/business/erp/report/fx-balance/fx-balance-logic.ts');
const API = sourceOf('src/api/business/erp/fx-balance.ts');
const ROUTER = sourceOf('src/router/index.ts');

/** 取 `<template>` 段：样式与脚本里的同名函数不算页面渲染契约 */
function templateOf(source: string): string {
  const start = source.indexOf('<template>');
  // 模板里有嵌套 `<template v-if="...">`：闭合标签取**最后**一个，否则会截到第一个内层 `</template>`
  const end = source.lastIndexOf('</template>');
  assert.ok(start >= 0 && end > start, '页面必须有 <template> 段');
  return source.slice(start, end);
}

const TPL = templateOf(PAGE);

// ==================== ① 路由 / 权限码 / 接口路径 ====================

test('菜单与路由：path / component / meta.permission 三处与迁移 V0412 逐字一致', () => {
  // 路径（V0412：`/business/erp/report/fx-balance`）
  assert.ok(ROUTER.includes("path: 'business/erp/report/fx-balance'"), '路由未注册 = 点菜单 404');
  // 组件路径（迁移里的 component 是 `business/erp/report/fx-balance/index`）
  assert.ok(ROUTER.includes("@/views/business/erp/report/fx-balance/index.vue"), '路由组件路径与 views 目录不一致');
  // 显形码 = 后端接口码（ERP 侧报表已解耦前，页面码就是接口码，见编码规范 §1.7.2 过渡期）
  assert.ok(ROUTER.includes("permission: 'business:erp:report:fx-balance'"), '路由 meta.permission 缺失 = 侧栏显形/前端守卫失效');
  assert.equal(PERM_FX_BALANCE, 'business:erp:report:fx-balance');
});

test('接口层：路径写全 /api（漏前缀会命中 SPA fallback 返回 HTML）', () => {
  assert.ok(API.includes("const BASE = '/api/business/erp'"), '接口前缀必须带 /api');
  assert.ok(API.includes('`${BASE}/report/fx-balance`'), '接口路径与后端路由不一致');
});

test('接口层：可空字段在类型上就是 null（缺牌价不是 0）', () => {
  for (const field of ['close_rate', 'converted_base', 'reval_diff']) {
    const block = API.slice(API.indexOf(` ${field}:`), API.indexOf(` ${field}:`) + 40);
    assert.match(block, new RegExp(`${field}: string \\| null`), `${field} 必须是 string | null`);
  }
  // 合计同理：任一币种缺牌价时后端给 null，前端类型不能写成 string
  assert.match(API, /total_converted_base: string \| null/);
  assert.match(API, /total_reval_diff: string \| null/);
  assert.match(API, /missing_rates: string\[\]/);
});

// ==================== ② 缺牌价：折算列必须走 null 感知函数 ====================

test('折算列：模板必须用 amountCell（null → 「—」），不许用 formatAmount（null → 空串）', () => {
  // 两处折算格（汇总行 + 明细行）
  const converted = TPL.match(/amountCell\((row|report)\.converted_base\)/g) ?? [];
  assert.ok(converted.length >= 2, `折算列必须两处都用 amountCell，实际 ${converted.length} 处`);
  // 折算列绝不能直接进 formatAmount（那会把 null 画成空串，再被补成 0.00）
  assert.doesNotMatch(TPL, /formatAmount\((row|report)\.converted_base\)/, '折算列走了 formatAmount = 缺牌价会被画成 0');
});

test('汇兑差额列：模板用 diffText（null → 「—」、正负 → 调增/调减）', () => {
  const diffs = TPL.match(/diffText\(row\.reval_diff\)/g) ?? [];
  assert.ok(diffs.length >= 1, '明细行的差额列必须用 diffText');
  assert.ok(TPL.includes('diffTotalText(report)'), 'KPI/合计的差额必须用 diffTotalText（null 感知）');
});

test('KPI 折算合计：用 convertedTotalText，不许把 null 合计画成 0', () => {
  assert.ok(TPL.includes('convertedTotalText(report)'), '折算合计必须走 convertedTotalText');
  assert.doesNotMatch(TPL, /formatAmount\(report\.total_converted_base\)/, '折算合计走了 formatAmount = 半套折算会被当成真数');
  assert.doesNotMatch(TPL, /formatAmount\(report\.total_reval_diff\)/, '差额合计走了 formatAmount');
});

test('缺牌价提示：页面上必须有一块点名币种 + 期末日的警告', () => {
  assert.ok(TPL.includes('v-if="missingRateMessage"'), '缺牌价告警没有 v-if 守卫（无缺牌价时会渲染空告警）');
  assert.ok(TPL.includes('data-testid="fxb-missing-rate"'), '缺牌价告警缺 data-testid（真机验收锚点）');
  assert.ok(PAGE.includes("missingRateText(report.value)"), '缺牌价文案必须由 missingRateText 生成（口径收在纯逻辑里）');
  // 文案必须说明「没有按汇率 1 折算」——这是本页最容易被人「修」成 1 的口径
  assert.match(LOGIC, /没有按汇率 1 折算/);
});

test('前端不算账：页面不做任何浮点折算（无 toFixed、无 * close_rate）', () => {
  assert.doesNotMatch(TPL, /toFixed\(/, '页面模板里出现 toFixed = 前端在自己折算/四舍五入');
  assert.doesNotMatch(PAGE, /toFixed\(/, '页面脚本里出现 toFixed = 金额被浮点化');
  assert.doesNotMatch(TPL, /\*\s*row\.close_rate/, '页面在乘汇率自己折算');
  assert.doesNotMatch(TPL, /\*\s*1(\.0+)?\b(?!\d)/, '页面在按汇率 1 折算');
});

// ==================== ③ 表格结构 ====================

test('明细表列：14 列且列名逐字固定（增删列要人判断，不许悄悄少一列）', () => {
  // 从明细表的第一段 <thead> 里取列名（明细段在汇总段之后）
  const tables = TPL.split('<thead>').map((chunk) => chunk.slice(0, chunk.indexOf('</thead>')));
  const headers = tables
    .filter((chunk) => chunk.includes('期初原币'))
    .flatMap((chunk) => Array.from(chunk.matchAll(/<th[^>]*>([\s\S]*?)<\/th>/g)).map((m) => m[1].replace(/<[^>]+>/g, '').replace(/\s+/g, ' ').trim()));
  assert.deepEqual(headers, [
    '科目',
    '币种',
    '期初原币',
    '本期借(原币)',
    '本期贷(原币)',
    '期末原币',
    '期初账面',
    '本期借(账面)',
    '本期贷(账面)',
    '期末账面',
    '期末即期汇率',
    '折算({{ report.base_currency }})',
    '汇兑差额',
    '账面口径',
  ]);
});

test('汇总段：按币种汇总 + 合计行，原币跨币种不相加', () => {
  assert.ok(TPL.includes('data-testid="fxb-summary"'), '缺「按币种汇总」段');
  assert.ok(TPL.includes('data-testid="fxb-summary-total"'), '缺汇总合计行');
  assert.ok(TPL.includes('原币跨币种不相加'), '合计行的原币列必须显式说明不相加（跨币种相加是错账）');
});

test('账面口径列：必须带 label + tooltip（多币种科目退化时要说清）', () => {
  assert.ok(TPL.includes('basisLabel(row.book_basis)'), '账面口径列必须走 basisLabel');
  assert.ok(TPL.includes('basisHint(row.book_basis)'), '账面口径列必须带 tooltip（否则用户不知道「该币种账面」什么意思）');
});

test('空表：区分「本来没有外币业务」与「筛了本位币」', () => {
  assert.ok(TPL.includes('emptyText(report, report.base_currency)'), '空表文案必须走 emptyText（区分本位币过滤）');
});

// ==================== ④ 逻辑边界（逻辑测已有的不重复，只补页面会踩的边） ====================

test('负数余额方向：−0.0001 也算贷方，-0 算平（不取绝对值、不四舍五入）', () => {
  assert.equal(sideOf('-0.0001'), 'CREDIT');
  assert.equal(sideOf('-0.0000'), 'ZERO');
  assert.equal(formatAmount('-0.0000'), '0.00', '负零不许显示成 -0.00');
});

test('缺牌价占位符：空串与 null 都进「—」，真 0 仍显示 0.00', () => {
  assert.equal(MISSING_RATE_PLACEHOLDER, '—');
  assert.equal(amountCell(''), '—');
  assert.equal(amountCell(null), '—');
  assert.equal(amountCell('0.0000'), '0.00');
});

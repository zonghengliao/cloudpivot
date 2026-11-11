/**
 * VTRACE-2：凭证详情「来源单据」格显示结论（`voucher/voucher-source-link.ts`）单测。
 *
 * 运行：`pnpm test:unit`。
 *
 * 这里钉死的不是「怎么跳」（那是 VTRACE-1 的 `resolveSourceRoute`），而是
 * **「这一格长什么样、点了会怎样、点不了时说了什么」**：
 *
 * 1. 该给链接的行给链接，且 `path` / `query` 与映射表逐字一致；
 * 2. 不该给链接的行（MANUAL / 未知词 / 页面未上线 / 缺主键）一律 `clickable === false`，
 *    而且四者的 tooltip **互不相同**（用户能看懂为什么点不了）；
 * 3. 可跳的目标页必须在 `router/index.ts` 里注册过（本仓路由手工注册，漏了就 404）；
 * 4. 链接文字永不为空（`by='id'` 的行没有单号时退化成「查看<目标名>」）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import { EXTRA_VOUCHER_SOURCES, SOURCE_ROUTE_TABLE } from '@/views/business/erp/shared/source-route';
import {
  MANUAL_NO_SOURCE_TIP,
  resolveSourceDocDisplay,
} from '@/views/business/erp/voucher/voucher-source-link';

/** 一个形状合法的 uuid（无业务含义）。 */
const ID = '018f3a52-0000-7000-8000-000000000001';

test('采购入库凭证 → 收货入库单详情，链接带 in_id', () => {
  const cell = resolveSourceDocDisplay('STOCK_IN', ID, 'IN-20261101-0001');
  assert.equal(cell.status, 'link');
  assert.equal(cell.clickable, true);
  assert.equal(cell.path, '/business/erp/stock-in');
  assert.deepEqual(cell.query, { in_id: ID });
  assert.equal(cell.deepLink, 'detail');
  assert.equal(cell.text, 'IN-20261101-0001', '链接文字用来源单号，不用 id');
  assert.equal(cell.tip, '打开收货入库单详情「IN-20261101-0001」');
});

test('`by=id` 的行没有单号时，链接文字退化成「查看<目标名>」（不留空链接）', () => {
  const cell = resolveSourceDocDisplay('STOCK_IN', ID, null);
  assert.equal(cell.clickable, true);
  assert.equal(cell.text, '查看收货入库单');
  assert.deepEqual(cell.query, { in_id: ID });

  // 空白单号同样按「没有单号」处理（后端历史数据里存在空格）
  assert.equal(resolveSourceDocDisplay('STOCK_IN', ID, '   ').text, '查看收货入库单');
});

test('出库凭证 → 出库页按单号筛（半深链，tooltip 承诺的是「定位」不是「开详情」）', () => {
  const cell = resolveSourceDocDisplay('STOCK_OUT', ID, 'OUT-20261101-0001');
  assert.equal(cell.clickable, true);
  assert.equal(cell.path, '/business/erp/stock-out');
  assert.deepEqual(cell.query, { out_no: 'OUT-20261101-0001' });
  assert.equal(cell.deepLink, 'filter');
  assert.equal(cell.tip, '打开出库单列表并按单号「OUT-20261101-0001」定位');
});

test('deepLink=none 的行：可以跳列表页，但 query 必须为空且 tooltip 不承诺定位', () => {
  const cell = resolveSourceDocDisplay('DEPRECIATION', ID, 'DEP-202611-0001');
  assert.equal(cell.clickable, true);
  assert.deepEqual(cell.query, {}, '页面不读参数时不能塞假参数');
  assert.equal(cell.tip, '打开折旧批次列表（该页暂不读取单号参数，需在列表里自行定位）');
});

test('手工凭证：纯文本 + 卡面指定 tooltip', () => {
  const cell = resolveSourceDocDisplay('MANUAL', null, null);
  assert.equal(cell.clickable, false);
  assert.equal(cell.status, 'manual');
  assert.equal(cell.path, null);
  assert.deepEqual(cell.query, {});
  assert.equal(cell.text, '—');
  assert.equal(cell.tip, MANUAL_NO_SOURCE_TIP);
  assert.equal(MANUAL_NO_SOURCE_TIP, '手工凭证，无来源单据');

  // 手工凭证即使带着来源单号（历史数据）也不给链接，但把单号照原样显示出来
  const withNo = resolveSourceDocDisplay('MANUAL', null, '手工-001');
  assert.equal(withNo.clickable, false);
  assert.equal(withNo.text, '手工-001');
  assert.equal(withNo.tip, MANUAL_NO_SOURCE_TIP);
});

test('OTHER / 未知词 / 空类型：都是纯文本，但理由各不相同', () => {
  const other = resolveSourceDocDisplay('OTHER', ID, 'X-1');
  assert.equal(other.clickable, false);
  assert.equal(other.status, 'unknown');
  assert.equal(other.tip, '来源类型未识别（OTHER），暂不支持跳转');

  const unknown = resolveSourceDocDisplay('PURCHASE', ID, 'PO-1');
  assert.equal(unknown.clickable, false);
  assert.equal(unknown.status, 'unknown');
  assert.equal(unknown.tip, '来源类型「PURCHASE」未识别，暂不支持跳转');
  assert.equal(unknown.text, 'PO-1', '未知来源也把单号显示出来（总比只给 — 好）');

  const empty = resolveSourceDocDisplay(null, null, null);
  assert.equal(empty.clickable, false);
  assert.equal(empty.status, 'empty');
  assert.equal(empty.tip, '无来源单据');
});

test('成本核算批次（COST_PERIOD）已可跳：落到成本核算页并带 batch_no（COST-B3-11）', () => {
  const cell = resolveSourceDocDisplay('COST_PERIOD', ID, 'CS-202611-0001');
  assert.equal(cell.clickable, true);
  assert.equal(cell.status, 'link');
  assert.equal(cell.path, '/business/erp/cost-period');
  assert.deepEqual(cell.query, { batch_no: 'CS-202611-0001' });
  assert.equal(cell.text, 'CS-202611-0001');

  // 没有批次号（来源单号缺失）⇒ 仍然不跳，理由说清缺的是哪个字段
  const noNo = resolveSourceDocDisplay('COST_PERIOD', ID, null);
  assert.equal(noNo.clickable, false);
  assert.equal(noNo.status, 'missing-key');
  assert.equal(noNo.tip, '本张凭证缺少「成本核算批次」的单据号，暂不能跳转');
});

test('缺定位参数：纯文本 + missing-key，并说明缺的是主键还是单号', () => {
  const noId = resolveSourceDocDisplay('STOCK_IN', null, 'IN-20261101-0001');
  assert.equal(noId.clickable, false);
  assert.equal(noId.status, 'missing-key');
  assert.equal(noId.tip, '本张凭证缺少「收货入库单」的单据主键，暂不能跳转');

  const noNo = resolveSourceDocDisplay('STOCK_OUT', ID, null);
  assert.equal(noNo.clickable, false);
  assert.equal(noNo.status, 'missing-key');
  assert.equal(noNo.tip, '本张凭证缺少「出库单」的单据号，暂不能跳转');
});

test('大小写 / 前后空格不改变结论', () => {
  const cell = resolveSourceDocDisplay('  stock_in ', ID, 'IN-1');
  assert.equal(cell.clickable, true);
  assert.deepEqual(cell.query, { in_id: ID });
});

test('全表不变量：可跳 ⇔ 映射表有解，且 page-missing / MANUAL 永不可跳', () => {
  const keys = [...Object.keys(SOURCE_ROUTE_TABLE), ...EXTRA_VOUCHER_SOURCES];
  assert.ok(keys.length >= 21, '映射表至少 21 行（20 个枚举值 + FX_REVALUATION）');

  for (const key of keys) {
    const spec = SOURCE_ROUTE_TABLE[key as keyof typeof SOURCE_ROUTE_TABLE];
    // 用最宽松的入参：id 与 no 都给，看能跳的行跳没跳
    const cell = resolveSourceDocDisplay(key, ID, sampleNoOf(key));

    assert.notEqual(cell.text, '', `${key}: 链接/文本文字不能为空`);
    assert.equal(
      cell.clickable,
      spec.deepLink !== 'page-missing' && spec.by !== 'none',
      `${key}: 可跳性与映射表不一致（deepLink=${spec.deepLink}, by=${spec.by}）`,
    );

    if (key === 'MANUAL') {
      assert.equal(cell.clickable, false, 'MANUAL 永远不可跳');
      assert.equal(cell.tip, MANUAL_NO_SOURCE_TIP);
    }
    if (spec.deepLink === 'page-missing') {
      assert.equal(cell.clickable, false, `${key}: 页面未上线不能给链接`);
      assert.equal(cell.status, 'page-missing');
    }
    if (cell.clickable) {
      assert.equal(cell.path, spec.path, `${key}: 落点必须是映射表里的 path`);
      assert.equal(cell.deepLink, spec.deepLink);
    }
  }
});

test('可跳的目标页必须在 router/index.ts 注册过（手工注册，漏了就是 404）', () => {
  const routerSource = readFileSync(new URL('../../src/router/index.ts', import.meta.url), 'utf8');
  const keys = [...Object.keys(SOURCE_ROUTE_TABLE), ...EXTRA_VOUCHER_SOURCES];

  for (const key of keys) {
    const spec = SOURCE_ROUTE_TABLE[key as keyof typeof SOURCE_ROUTE_TABLE];
    const cell = resolveSourceDocDisplay(key, ID, sampleNoOf(key));
    if (!cell.clickable || !cell.path) {
      continue;
    }
    const routePath = cell.path.replace(/^\//, '');
    assert.ok(
      routerSource.includes(`path: '${routePath}'`),
      `${key}: 目标页 ${cell.path} 没在 router/index.ts 注册`,
    );
  }
});

/**
 * 给某一行造一个该行 `format` 能接受的单号（见 `source-route.ts`：只有 OPENING / YEAR_END
 * 两行的 `format` 要求特定前缀）。给不出格式的行 `format` 会返回 `null` ⇒ 该行这次显示成
 * `missing-key`，不变量「可跳 ⇔ 映射表有解」仍然成立，所以不影响断言强度。
 */
const SAMPLE_NO: Record<string, string> = {
  OPENING: 'OPENING-2026-01',
  YEAR_END: 'YEAR-END-2026',
};

function sampleNoOf(key: string): string {
  return SAMPLE_NO[key] ?? 'DOC-20261101-0001';
}

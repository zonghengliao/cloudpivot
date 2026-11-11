/**
 * VTRACE-1：凭证来源 → 前端路由映射表（`shared/source-route.ts`）单测。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * 为什么值得钉死：`resolveSourceRoute()` 是「凭证 → 原单据」唯一入口，跳错了用户会
 * 落到**别的单据详情**（比跳不动更糟）。所以这里断言三件事：
 *
 * 1. **覆盖完整**：表键集合 == 后端 `VoucherSourceType::ALL`（前端镜像
 *    `VOUCHER_SOURCE_TYPES`）**加** `FX_REVALUATION`，一个不多一个不少；
 * 2. **每条落点正确**：每个来源解析出的 path / deepLink 逐条写死（后端加来源时这里先红）；
 * 3. **不给假链接**：MANUAL / 未知词 / 参数缺失 / 页面未落地 一律 `null`，
 *    且 `deepLink='none'` 的行绝不在 query 里塞目标页不读的参数。
 *
 * 另外用 `router/index.ts` 的源文本兜一道：凡是 `deepLink !== 'page-missing'` 的 path
 * 都必须在路由表里注册（本仓路由**手工注册**，漏了 = 点进去 404）；
 * `page-missing` 的 path 反过来必须**还没注册** —— 它一旦被注册，就该把这行改成能跳。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import { VOUCHER_SOURCE_TYPES } from '@/api/business/erp/doc-kinds';
import {
  EXTRA_VOUCHER_SOURCES,
  SOURCE_ROUTE_TABLE,
  isSourceDocType,
  resolveSourceRoute,
  type DeepLink,
} from '@/views/business/erp/shared/source-route';

/** 一个能跑通 `by='id'` 的 uuid（无业务含义）。 */
const ID = '018f3a52-0000-7000-8000-000000000001';

/**
 * 期望表：表里**每一行**都必须在下面出现（`表键集合 == 期望表键集合` 断言兜底）。
 * `path` / `deepLink` 是本卡验收口径；`query` 写死的是「解析出来后真能用的参数」。
 */
const EXPECTED: Record<
  string,
  { path: string; deepLink: DeepLink; sample: string; query: Record<string, string> }
> = {
  STOCK_IN: { path: '/business/erp/stock-in', deepLink: 'detail', sample: ID, query: { in_id: ID } },
  STOCK_OUT: {
    path: '/business/erp/stock-out',
    deepLink: 'filter',
    sample: 'OUT-20261101-0001',
    query: { out_no: 'OUT-20261101-0001' },
  },
  PAYMENT: { path: '/business/erp/payable', deepLink: 'none', sample: 'PAY-20261101-0001', query: {} },
  STOCK_TAKE: { path: '/business/erp/stock-take', deepLink: 'none', sample: ID, query: {} },
  SERIAL_BATCH: { path: '/business/erp/serial', deepLink: 'none', sample: 'SN-B-0001', query: {} },
  SERIAL_OUT: { path: '/business/erp/serial', deepLink: 'none', sample: 'SN-O-0001', query: {} },
  SERIAL_SCRAP: { path: '/business/erp/serial', deepLink: 'none', sample: 'SN-S-0001', query: {} },
  OPENING: { path: '/business/erp/opening', deepLink: 'none', sample: 'OPENING-2026-01', query: {} },
  YEAR_END: { path: '/business/erp/year-end', deepLink: 'detail', sample: 'YEAR-END-2026', query: { year: '2026' } },
  MANUAL: { path: '', deepLink: 'none', sample: '', query: {} },
  FIXED_ASSET_ACQ: { path: '/business/erp/asset', deepLink: 'none', sample: ID, query: {} },
  FIXED_ASSET_DISPOSAL: { path: '/business/erp/asset', deepLink: 'none', sample: ID, query: {} },
  DEPRECIATION: { path: '/business/erp/depreciation', deepLink: 'none', sample: ID, query: {} },
  EXPENSE_REPORT: { path: '/business/erp/expense', deepLink: 'none', sample: ID, query: {} },
  EXPENSE_PAYMENT: { path: '/business/erp/expense', deepLink: 'none', sample: ID, query: {} },
  RECEIPT: { path: '/business/erp/receipt', deepLink: 'none', sample: ID, query: {} },
  RECEIPT_REFUND: { path: '/business/erp/receipt-refund', deepLink: 'none', sample: ID, query: {} },
  INVOICE: { path: '/business/erp/invoice', deepLink: 'none', sample: ID, query: {} },
  COMMISSION: {
    path: '/business/erp/commission',
    deepLink: 'none',
    sample: 'CMS-202609-0001',
    query: {},
  },
  COST_PERIOD: { path: '/business/erp/cost-period', deepLink: 'page-missing', sample: ID, query: {} },
  FX_REVALUATION: { path: '/business/erp/period', deepLink: 'none', sample: '2026-01', query: {} },
};

/** 按映射表的 `by` 决定把 sample 放 id 还是 no。 */
function resolveRow(key: keyof typeof SOURCE_ROUTE_TABLE, sample: string) {
  const spec = SOURCE_ROUTE_TABLE[key];
  return resolveSourceRoute(key, spec.by === 'id' ? sample : null, spec.by === 'no' ? sample : null);
}

test('覆盖完整：表键 == 后端枚举 20 种 + FX_REVALUATION，一个不多一个不少', () => {
  const tableKeys = Object.keys(SOURCE_ROUTE_TABLE).sort();
  const expectedKeys = [...VOUCHER_SOURCE_TYPES, ...EXTRA_VOUCHER_SOURCES].sort();

  assert.deepEqual(tableKeys, expectedKeys, '映射表与 VoucherSourceType 枚举 + EXTRA 必须逐字对齐');
  assert.equal(VOUCHER_SOURCE_TYPES.length, 20, '后端 VoucherSourceType::ALL 是 20 种');
  assert.equal(tableKeys.length, 21, '20 种枚举 + FX_REVALUATION = 21 行');
  assert.deepEqual(Object.keys(EXPECTED).sort(), tableKeys, '每个键都要有本文件的期望行（防漏测）');
  assert.equal(new Set(tableKeys).size, tableKeys.length, '键不可重复');
});

test('每条来源都解析出预期 path / deepLink / query', () => {
  for (const [key, want] of Object.entries(EXPECTED)) {
    const got = resolveRow(key as keyof typeof SOURCE_ROUTE_TABLE, want.sample);
    if (key === 'MANUAL' || want.deepLink === 'page-missing') {
      assert.equal(got, null, `${key} 应不可跳（${want.deepLink}）`);
      continue;
    }
    assert.ok(got, `${key} 应能跳到 ${want.path}`);
    assert.equal(got.path, want.path, `${key} 目标页`);
    assert.equal(got.deepLink, want.deepLink, `${key} 深链程度`);
    assert.deepEqual(got.query, want.query, `${key} 查询参数`);
    assert.ok(got.label.length > 0, `${key} 必须有中文目标名（供 tooltip / aria-label）`);
  }
});

test('MANUAL / OTHER（卡面词）/ 未知词 / 空值 一律 null，绝不猜页面', () => {
  assert.equal(resolveSourceRoute('MANUAL', ID, 'MANUAL-001'), null);
  // 卡面 body 里的 MANUAL / OTHER 是这条轴上的「不可跳」；OTHER 根本不是凭证来源。
  assert.equal(resolveSourceRoute('OTHER', ID, 'X-1'), null);
  assert.equal(resolveSourceRoute('', ID, 'X-1'), null);
  assert.equal(resolveSourceRoute(null, ID, 'X-1'), null);
  assert.equal(resolveSourceRoute(undefined, ID, 'X-1'), null);
  assert.equal(resolveSourceRoute('   ', ID, 'X-1'), null);
  assert.equal(resolveSourceRoute('NOT_A_SOURCE', ID, 'X-1'), null);
  // 大小写不敏感：库里是大写，但按钮传进来小写也不该失效。
  assert.equal(resolveSourceRoute('stock_in', ID, null)?.path, '/business/erp/stock-in');
  // 前后空格同理（单号类字段抄过来常带空格）。
  assert.equal(resolveSourceRoute(' STOCK_IN ', ID, null)?.path, '/business/erp/stock-in');
});

test('卡面 body 里属于「别的轴」的词不造跳转（PURCHASE / MATERIAL / LABOR …）', () => {
  // 这些是 biz_material_lot.source_type / 成本归集来源，不是 biz_voucher.source_type；
  // 给它们硬造一条凭证跳转＝跳到错页面（详见模块头注释的对照表）。
  const otherAxis = [
    'PURCHASE',
    'RETURN',
    'PRODUCTION',
    'MATERIAL',
    'LABOR',
    'LABOR_TICKET',
    'OVERHEAD',
    'OVERHEAD_TICKET',
    'ALLOC',
    'IMPORT',
    'DEPRECIATION_REVERSAL',
    'EXPENSE',
  ];
  for (const word of otherAxis) {
    assert.equal(isSourceDocType(word), false, `${word} 不该被当成凭证来源`);
    assert.equal(resolveSourceRoute(word, ID, 'X-1'), null, `${word} 不该跳`);
    // 也没被悄悄塞进表里（否则 by/queryKey 会指向某个真实页面，等于给假链接）。
    assert.equal(Object.prototype.hasOwnProperty.call(SOURCE_ROUTE_TABLE, word), false);
  }
});

test('参数缺失 / 格式不对 → null（宁可不给链接，不给错链接）', () => {
  // by='id' 的行：id 空则不跳（哪怕 no 有值）
  assert.equal(resolveSourceRoute('STOCK_IN', null, 'IN-20261101-0001'), null);
  assert.equal(resolveSourceRoute('STOCK_IN', '', 'IN-20261101-0001'), null);
  assert.equal(resolveSourceRoute('STOCK_IN', '   ', 'IN-20261101-0001'), null);
  // by='no' 的行：no 空则不跳
  assert.equal(resolveSourceRoute('STOCK_OUT', ID, null), null);
  assert.equal(resolveSourceRoute('STOCK_OUT', ID, '  '), null);
  // OPENING 只认 'OPENING-<期间>' 前缀，别的单号一律不跳（不猜）
  const opening = resolveSourceRoute('OPENING', null, 'OPENING-2026-01');
  assert.equal(opening?.path, '/business/erp/opening');
  // 期初页还没读 period ⇒ deepLink=none，参数不塞（格式仍参与判定：摘不出期间就不跳）
  assert.deepEqual(opening?.query, {});
  assert.equal(resolveSourceRoute('OPENING', null, 'OE-2026-01'), null);
  assert.equal(resolveSourceRoute('OPENING', null, 'OPENING-'), null);
  // YEAR_END 只认 'YEAR-END-<4 位年>'，年份不是 4 位数字则不跳
  assert.equal(resolveSourceRoute('YEAR_END', null, 'YEAR-END-2026')?.query.year, '2026');
  assert.equal(resolveSourceRoute('YEAR_END', null, 'YEAR-END-26'), null);
  assert.equal(resolveSourceRoute('YEAR_END', null, '2026'), null);
});

test('deepLink 语义：none 不塞参数、page-missing 整条不返回', () => {
  // detail：参数必须塞进去（否则页面读不到）
  assert.deepEqual(resolveSourceRoute('STOCK_IN', ID, null)?.query, { in_id: ID });
  assert.deepEqual(resolveSourceRoute('YEAR_END', null, 'YEAR-END-2026')?.query, { year: '2026' });
  // filter：参数塞进去当筛选条件
  assert.deepEqual(resolveSourceRoute('STOCK_OUT', null, 'OUT-1')?.query, { out_no: 'OUT-1' });
  // none：页面还没读这个参数 ⇒ query 恒空（塞了就是个静默无效的假参数）
  for (const key of ['PAYMENT', 'STOCK_TAKE', 'SERIAL_BATCH', 'EXPENSE_REPORT', 'COMMISSION'] as const) {
    const spec = SOURCE_ROUTE_TABLE[key];
    assert.equal(spec.deepLink, 'none');
    assert.ok(spec.queryKey, `${key} 要记下「该补哪个参数」`);
    const got = resolveRow(key, key === 'COMMISSION' ? 'CMS-1' : spec.by === 'id' ? ID : 'NO-1');
    assert.deepEqual(got?.query, {}, `${key} deepLink=none ⇒ query 必须为空`);
    assert.equal(got?.path, spec.path, `${key} 仍应落到列表页（可用）`);
  }
  // page-missing：路由都没注册 ⇒ 返回 null，不给 404 链接
  assert.equal(SOURCE_ROUTE_TABLE.COST_PERIOD.deepLink, 'page-missing');
  assert.equal(resolveSourceRoute('COST_PERIOD', ID, 'CS-202611-0001'), null);
});

test('每个非空 path 都在 router/index.ts 注册；page-missing 的反之还没注册', () => {
  const routerSrc = readFileSync(new URL('../../src/router/index.ts', import.meta.url), 'utf8');
  const registered = new Set(
    [...routerSrc.matchAll(/path:\s*'([^']+)'/g)].map((m) => `/${m[1].replace(/^\/+/, '')}`),
  );

  for (const [key, spec] of Object.entries(SOURCE_ROUTE_TABLE)) {
    if (!spec.path) {
      continue; // MANUAL：无目标页
    }
    if (spec.deepLink === 'page-missing') {
      assert.equal(
        registered.has(spec.path),
        false,
        `${key} 标了 page-missing，但 ${spec.path} 已在路由表注册 —— 请把 deepLink 改成 detail/filter 并补页面读取`,
      );
      continue;
    }
    assert.equal(registered.has(spec.path), true, `${key} 目标页 ${spec.path} 未在 router/index.ts 注册（会 404）`);
  }
});

test('by / queryKey / label / note 的形态约束', () => {
  for (const [key, spec] of Object.entries(SOURCE_ROUTE_TABLE)) {
    assert.ok(spec.label.trim().length > 0, `${key} 缺中文名`);
    assert.ok(spec.note.trim().length > 0, `${key} 缺「为什么是这个页面」的 note`);
    if (spec.by === 'none') {
      // 只有「不可跳」的行才允许 by='none'
      assert.equal(key, 'MANUAL', `${key} 用 by='none' 却不在不可跳名单里`);
      assert.equal(spec.queryKey, null);
      assert.equal(spec.path, '');
    } else {
      assert.ok(spec.queryKey, `${key} 必须写明目标页参数名`);
      assert.ok(spec.path.startsWith('/business/erp/'), `${key} path 必须是 /business/erp/ 下的绝对路径`);
    }
  }
});

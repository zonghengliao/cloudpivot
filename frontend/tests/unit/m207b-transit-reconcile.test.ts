/**
 * M2-07b（t_4e93fa53）：在途对账页**契约单测**（差异行 + 汇总）。
 *
 * 运行：`cd frontend && pnpm test:unit`（`tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 两层断言（照 M2-07 的路子）
 *
 * 1. **纯逻辑**（`transit-logic.ts` 的对账段）：拼参（`include_matched` 只在勾上时才发、
 *    日期区间拆两端、空串整键缺席）、差异码主题、**结论中文只取后端 `issue_labels`**、
 *    三种空态（未查 / 筛选下无差异 / 全部对平）、结论横幅、截断提示、403 人话。
 * 2. **源码即契约**（`index.vue` / `transit-logic.ts` / API 层 / 发货单页深链）：
 *    真正会静默出错的是「页面把账算了一遍」—— 逻辑函数全绿、单测全过，
 *    页面却在 `reduce` 里加总、或者自己 `item_qty - received - transit` 求差额。
 *
 * 只读源码做子串 / 正则断言，不引 DOM、不引浏览器。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

import {
  PERM_TRANSIT_AUDIT,
  RECONCILE_TAB,
  TRANSIT_TABS,
  buildReconcileQuery,
  diffText,
  emptyReconcileFilter,
  isReconcileFiltered,
  isZeroQty,
  issueText,
  issueTheme,
  reconcileBanner,
  reconcileEmptyText,
  reconcileErrorMessage,
  reconcileResultText,
  reconcileResultTheme,
  reconcileRowKey,
  reconcileTruncatedNote,
  reconcileWarehouseText,
} from '@/views/business/erp/inventory-transit/transit-logic';
import type {
  TransitReconcile,
  TransitReconcileRow,
} from '@/api/business/erp/inventory-transit';

const ROOT = resolve(process.cwd());

function sourceOf(relative: string): string {
  return readFileSync(join(ROOT, relative), 'utf8');
}

/** 去掉注释再断言实现约束：文档里为了解释「为什么」会提到被禁止的写法本身 */
function withoutComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\//g, '').replace(/(^|[^:])\/\/.*$/gm, '$1');
}

/** 模板注释（`<!-- -->`）里也会写到被约束的写法，计数前先摘掉 */
function withoutHtmlComments(source: string): string {
  return source.replace(/<!--[\s\S]*?-->/g, '');
}

/** 取 `<template>` 段：脚本里的同名函数不算页面渲染契约 */
function templateOf(source: string): string {
  const start = source.indexOf('<template>');
  const end = source.lastIndexOf('</template>');
  assert.ok(start >= 0 && end > start, '页面必须有 <template> 段');
  return source.slice(start + '<template>'.length, end);
}

const PAGE_PATH = 'src/views/business/erp/inventory-transit/index.vue';
const LOGIC_PATH = 'src/views/business/erp/inventory-transit/transit-logic.ts';
const API_PATH = 'src/api/business/erp/inventory-transit.ts';
const SHIPMENT_PAGE_PATH = 'src/views/business/erp/shipment/index.vue';
/** 后端：差异码与文案的唯一来源（逐字比对的对照物） */
const RECONCILE_RS_PATH = '../backend/app/src/business/erp/inventory_transit/reconcile.rs';

const PAGE = sourceOf(PAGE_PATH);
const PAGE_CODE = withoutComments(PAGE);
const TPL = templateOf(PAGE);
const LOGIC = sourceOf(LOGIC_PATH);
const LOGIC_CODE = withoutComments(LOGIC);
const API = sourceOf(API_PATH);
const SHIPMENT_PAGE = sourceOf(SHIPMENT_PAGE_PATH);
const RECONCILE_RS = sourceOf(RECONCILE_RS_PATH);

// ==================== 假报告（字段逐字对齐后端 DTO） ====================

function row(overrides: Partial<TransitReconcileRow> = {}): TransitReconcileRow {
  return {
    shipment_id: '11111111-1111-1111-1111-111111111111',
    ship_no: 'M207B-SH1',
    shipment_status: 'POSTED',
    shipment_deleted: false,
    ship_date: '2026-11-10',
    material_id: '22222222-2222-2222-2222-222222222222',
    material_code: 'MAT-001',
    material_name: '螺丝',
    spec: 'M6',
    unit_name: '个',
    warehouse_id: '33333333-3333-3333-3333-333333333333',
    warehouse_code: 'WH-001',
    warehouse_name: '主仓',
    item_qty: '10.0000',
    shipped_qty: '10.0000',
    received_qty: '4.0000',
    qty_in_transit: '5.0000',
    cancelled_qty: '0.0000',
    diff: '1.0000',
    issues: ['QTY_MISMATCH'],
    issue_labels: ['发货单明细的发货量与在途账记的发货量不符'],
    matched: false,
    ...overrides,
  };
}

function report(
  overrides: Partial<Omit<TransitReconcile, 'totals'>> & { totals?: Partial<TransitReconcile['totals']> } = {},
): TransitReconcile {
  const totals = {
    checked: 3,
    matched: 1,
    mismatched: 2,
    cancelled: 0,
    untracked: 0,
    item_qty: '30.0000',
    shipped_qty: '30.0000',
    received_qty: '12.0000',
    in_transit_qty: '17.0000',
    cancelled_qty: '0.0000',
    untracked_item_qty: '0.0000',
    diff: '1.0000',
  };
  const merged = { ...totals, ...(overrides.totals ?? {}) };
  return {
    checked: 3,
    matched: 1,
    mismatched: 2,
    balanced: false,
    truncated: false,
    scan_limit: 2000,
    rows: [row()],
    ...overrides,
    totals: merged,
  };
}

// ==================== ① 拼参 ====================

test('对账拼参：空筛选不发任何键（include_matched 也不发 = 后端默认只看差异组）', () => {
  const query = buildReconcileQuery(emptyReconcileFilter());
  assert.deepEqual(query, {});
  assert.equal('include_matched' in query, false, '缺省不发：发 false 是等价但多余，发空串会被后端当「等于空串」');
  assert.equal('ship_date_from' in query, false);
  assert.equal('shipment_id' in query, false);
});

test('对账拼参：勾上「连对平的组一起看」才发 include_matched=true（字符串，不是布尔）', () => {
  const query = buildReconcileQuery({ ...emptyReconcileFilter(), include_matched: true });
  assert.equal(query.include_matched, 'true');
  assert.equal(typeof query.include_matched, 'string', '后端收 String（true/false/1/0/yes/no/on/off），布尔会被 422');
});

test('对账拼参：日期区间拆成 ship_date_from / ship_date_to（只给一端就只发一端）', () => {
  const both = buildReconcileQuery({
    ...emptyReconcileFilter(),
    date_range: ['2026-11-01', '2026-11-16'],
  });
  assert.equal(both.ship_date_from, '2026-11-01');
  assert.equal(both.ship_date_to, '2026-11-16');

  const onlyFrom = buildReconcileQuery({ ...emptyReconcileFilter(), date_range: ['2026-11-01', ''] });
  assert.equal(onlyFrom.ship_date_from, '2026-11-01');
  assert.equal('ship_date_to' in onlyFrom, false);
});

test('对账拼参：发货单 / 物料 / 仓库三个 id 原样透传，空串整键缺席', () => {
  const query = buildReconcileQuery({
    ...emptyReconcileFilter(),
    shipment_id: 'ship-uuid',
    material_id: '',
    warehouse_id: 'wh-uuid',
  });
  assert.equal(query.shipment_id, 'ship-uuid');
  assert.equal(query.warehouse_id, 'wh-uuid');
  assert.equal('material_id' in query, false);
});

test('「有没有在筛」不看 include_matched（它只改返回集合大小，不改口径）', () => {
  assert.equal(isReconcileFiltered(emptyReconcileFilter()), false);
  assert.equal(isReconcileFiltered({ ...emptyReconcileFilter(), include_matched: true }), false);
  assert.equal(isReconcileFiltered({ ...emptyReconcileFilter(), shipment_id: 'x' }), true);
  assert.equal(isReconcileFiltered({ ...emptyReconcileFilter(), date_range: ['2026-11-01', ''] }), true);
});

// ==================== ② 差异码：只上色，不译字 ====================

test('五个硬问题码 → danger；CANCELLED_ONLY → warning；未知码 → default', () => {
  for (const issue of [
    'QTY_MISMATCH',
    'MISSING_TRANSIT',
    'ORPHAN_TRANSIT',
    'IDENTITY_BROKEN',
    'MULTI_WAREHOUSE',
  ]) {
    assert.equal(issueTheme(issue), 'danger', `${issue} 是硬问题，必须醒目`);
  }
  assert.equal(issueTheme('CANCELLED_ONLY'), 'warning', '整批作废不算「不平」，用警告色而不是红色');
  assert.equal(issueTheme('NOPE'), 'default');
  assert.equal(issueTheme(null), 'default');
});

test('前端常量与后端差异码逐字一致（后端加/改码，这里先红）', () => {
  const codes = [
    'QTY_MISMATCH',
    'MISSING_TRANSIT',
    'ORPHAN_TRANSIT',
    'IDENTITY_BROKEN',
    'MULTI_WAREHOUSE',
    'CANCELLED_ONLY',
  ];
  for (const code of codes) {
    assert.ok(RECONCILE_RS.includes(`ISSUE_${code}: &str = "${code}"`), `后端没有差异码常量 ${code}`);
    assert.ok(LOGIC.includes(code), `前端逻辑缺差异码 ${code}（上色会退化成 default）`);
  }
});

test('结论文案逐字取后端 issue_labels（前端一个字都不译）', () => {
  const mismatch = row();
  assert.equal(issueText(mismatch), '发货单明细的发货量与在途账记的发货量不符');

  const two = row({
    issues: ['QTY_MISMATCH', 'MULTI_WAREHOUSE'],
    issue_labels: ['发货单明细的发货量与在途账记的发货量不符', '同一发货单同一物料在多仓有在途行（同一批货被记了两份）'],
  });
  assert.equal(
    issueText(two),
    '发货单明细的发货量与在途账记的发货量不符；同一发货单同一物料在多仓有在途行（同一批货被记了两份）',
  );

  assert.equal(issueText(row({ matched: true, issues: [], issue_labels: [] })), '对平');
  // matched=false 但后端没给文案 → 与后端 issue_label 的兜底同字（不新造）
  assert.equal(issueText(row({ matched: false, issues: ['SOMETHING_NEW'], issue_labels: [] })), '未知差异');
  assert.ok(RECONCILE_RS.includes('"未知差异"'), '兜底文案必须与后端 issue_label 的兜底一致');
});

test('前端源码里没有抄后端差异文案（抄了就会漂）', () => {
  // 这些是 reconcile.rs::issue_label 的字面量；前端只许用 issue_labels 字段，不许内置文本
  const backendLabels = [
    '货发了没进在途',
    '在途账有这批货，但发货单明细里没有对应物料',
    '在途行自身不满足',
    '同一批货被记了两份',
    '只有在途作废行，不计在途也不计已出',
  ];
  for (const label of backendLabels) {
    assert.ok(RECONCILE_RS.includes(label), `后端文案变了：${label}（同步本测试）`);
    assert.equal(LOGIC_CODE.includes(label), false, `逻辑层不许内置后端文案：${label}`);
    assert.equal(PAGE_CODE.includes(label), false, `页面不许内置后端文案：${label}`);
  }
});

test('行判定三态：对平 / 不平 / 整批作废（作废不算不平，与后端 mismatched 口径一致）', () => {
  assert.equal(reconcileResultText(row({ matched: true, issues: [] })), '对平');
  assert.equal(reconcileResultTheme(row({ matched: true, issues: [] })), 'success');

  assert.equal(reconcileResultText(row({ issues: ['QTY_MISMATCH'] })), '不平');
  assert.equal(reconcileResultTheme(row({ issues: ['QTY_MISMATCH'] })), 'danger');

  assert.equal(reconcileResultText(row({ issues: ['CANCELLED_ONLY'] })), '整批作废');
  assert.equal(reconcileResultTheme(row({ issues: ['CANCELLED_ONLY'] })), 'warning');

  // 作废 + 硬问题 → 仍是「不平」（先判硬问题）
  assert.equal(reconcileResultText(row({ issues: ['CANCELLED_ONLY', 'QTY_MISMATCH'] })), '不平');
});

// ==================== ③ 数量 / 差额 ====================

test('差额直接显示后端 diff（字符串 Decimal 去尾零、负数保留符号）', () => {
  assert.equal(diffText({ diff: '1.0000' }), '1');
  assert.equal(diffText({ diff: '-3.5000' }), '-3.5');
  assert.equal(diffText({ diff: '0.0000' }), '0');
  assert.equal(diffText({ diff: '1234.2500' }), '1,234.25');
  assert.equal(diffText({ diff: 0 }), '0');
});

test('isZeroQty 判「恰好为 0」：字符串 Decimal 安全，负数不算 0', () => {
  assert.equal(isZeroQty('0.0000'), true);
  assert.equal(isZeroQty(0), true);
  assert.equal(isZeroQty(null), true);
  assert.equal(isZeroQty(''), true);
  assert.equal(isZeroQty('-0.0000'), true);
  assert.equal(isZeroQty('0.0001'), false);
  assert.equal(isZeroQty('-1'), false);
  assert.equal(isZeroQty('1'), false);
});

test('对账行键 = 发货单 × 物料（后端 ComboRow 的 GROUP BY 就是这两列）', () => {
  assert.equal(reconcileRowKey(row()), `${row().shipment_id}|${row().material_id}`);
});

test('仓库列：warehouse_id=null 是「这组没有在途行」，不是「仓库已删除」', () => {
  assert.equal(reconcileWarehouseText(row({ warehouse_id: null })), '—（无在途行）');
  assert.equal(reconcileWarehouseText(row()), 'WH-001 主仓');
  assert.equal(reconcileWarehouseText(row({ warehouse_code: null, warehouse_name: null })), '（仓库已删除）');
});

// ==================== ④ 三种空态分开写 ====================

test('空态①：还没跑过对账 → 引导「开始对账」', () => {
  const text = reconcileEmptyText(null, emptyReconcileFilter());
  assert.match(text.title, /还没有对账结果/);
  assert.match(text.desc, /开始对账/);
});

test('空态②：全部对平（checked>0、无筛选）—— 与「没数据」分开写', () => {
  const balanced = report({
    balanced: true,
    rows: [],
    matched: 3,
    mismatched: 0,
    totals: { checked: 3, matched: 3, mismatched: 0, diff: '0.0000' },
  });
  const text = reconcileEmptyText(balanced, emptyReconcileFilter());
  assert.match(text.title, /全部对平（3 组）/);
  assert.match(text.desc, /这一页空着是好消息/);
  assert.notEqual(text.title, reconcileEmptyText(null, emptyReconcileFilter()).title);
});

test('空态③：一张可对账的发货单都没有 → 说明对账只覆盖 POSTED 单', () => {
  const empty = report({
    checked: 0,
    matched: 0,
    mismatched: 0,
    balanced: true,
    rows: [],
    totals: { checked: 0, matched: 0, mismatched: 0 },
  });
  const text = reconcileEmptyText(empty, emptyReconcileFilter());
  assert.match(text.title, /还没有可对账的发货单/);
  assert.match(text.desc, /POSTED/);
});

test('空态④：筛选下无差异 ≠ 全部对平（文案要指回「连对平的组一起看」）', () => {
  const filtered = { ...emptyReconcileFilter(), material_id: 'mat-uuid' };
  const text = reconcileEmptyText(
    report({ checked: 2, rows: [], mismatched: 0, balanced: true, totals: { checked: 2, matched: 2, mismatched: 0 } }),
    filtered,
  );
  assert.match(text.title, /当前筛选下没有差异组（2 组全对平）/);
  assert.match(text.desc, /连对平的组一起看/);
  // 与「全部对平」的空态是两句话：用户要知道「是筛出来的空」还是「账本来就平」
  assert.notEqual(
    text.title,
    reconcileEmptyText(report({ rows: [], mismatched: 0, balanced: true }), emptyReconcileFilter()).title,
  );
});

// ==================== ⑤ 结论横幅：只用后端字段 ====================

test('横幅：balanced=true → success，三侧合计与差额逐字取 totals', () => {
  const banner = reconcileBanner(
    report({ balanced: true, checked: 4, matched: 4, mismatched: 0, totals: { checked: 4, matched: 4, mismatched: 0, item_qty: '40.0000', received_qty: '10.0000', in_transit_qty: '30.0000', diff: '0.0000' } }),
  );
  assert.ok(banner);
  assert.equal(banner.theme, 'success');
  assert.match(banner.title, /全部对平/);
  assert.match(banner.title, /4 组/);
  assert.match(banner.desc, /发货总量 40 = 已出 10 \+ 在途 30/);
  assert.match(banner.desc, /差额 0/);
});

test('横幅：有硬问题 → error（差额为 0 也照样报，不能拿差额当唯一判据）', () => {
  const banner = reconcileBanner(
    report({ balanced: false, mismatched: 2, totals: { checked: 3, matched: 1, mismatched: 2, diff: '0.0000' } }),
  );
  assert.ok(banner);
  assert.equal(banner.theme, 'error');
  assert.match(banner.title, /有 2 组不平/);
});

test('横幅：零硬问题但未对平 → warning（截断 / 差额）', () => {
  const banner = reconcileBanner(
    report({
      balanced: false,
      truncated: true,
      mismatched: 0,
      totals: { checked: 3, matched: 3, mismatched: 0, diff: '5.0000' },
    }),
  );
  assert.ok(banner);
  assert.equal(banner.theme, 'warning');
  assert.match(banner.title, /整体未对平/);
  assert.match(banner.title, /差额 5/);
});

test('横幅：没跑过对账 → 不渲染', () => {
  assert.equal(reconcileBanner(null), null);
});

test('横幅不自己算账：totals 故意不自洽时，仍原样显示后端给的数', () => {
  // item_qty 999 ≠ received 1 + in_transit 1，diff 也「算不平」——
  // 若前端自己相减，横幅会显示 2 / 6；取后端字段则显示 999 / 997。
  const banner = reconcileBanner(
    report({
      balanced: false,
      mismatched: 1,
      totals: { checked: 1, matched: 0, mismatched: 1, item_qty: '999.0000', received_qty: '1.0000', in_transit_qty: '1.0000', diff: '997.0000' },
    }),
  );
  assert.ok(banner);
  assert.match(banner.desc, /发货总量 999/);
  assert.match(banner.desc, /对账差额 997/);
  assert.equal(banner.desc.includes('2 = '), false);
});

// ==================== ⑥ 截断 / 403 人话 ====================

test('截断提示：说清「行不全 **且** 合计也只覆盖已扫描组」（对账与汇总的口径不同）', () => {
  assert.equal(reconcileTruncatedNote(report({ truncated: false })), '');
  assert.equal(reconcileTruncatedNote(null), '');
  const note = reconcileTruncatedNote(report({ truncated: true, scan_limit: 2000 }));
  assert.match(note, /上限 2000 组/);
  assert.match(note, /合计与差额同样只统计已扫描的组/);
});

test('403 人话点名缺 :audit 码（与取数码分开）', () => {
  const text = reconcileErrorMessage(new Error('403 无权限'));
  assert.ok(text.includes(PERM_TRANSIT_AUDIT), '文案必须点名对账码');
  assert.match(text, /与取数码 business:erp:inventory-transit:list 是两个码/i);
  assert.equal(reconcileErrorMessage(new Error('查询超时')), '查询超时');
  assert.match(reconcileErrorMessage(null), /没有返回原因/);
});

// ==================== ⑦ 页面源码契约 ====================

test('第三个 Tab 由 :audit 码判显隐，只影响自己（不整页 403）', () => {
  assert.equal(RECONCILE_TAB.key, 'reconcile');
  assert.equal(RECONCILE_TAB.label, '在途对账');
  assert.match(PAGE_CODE, /hasPermission\(PERM_TRANSIT_AUDIT\)/);
  assert.match(TPL, /<t-tab-panel v-if="canAudit"[^>]*:value="RECONCILE_TAB\.key"/);
  assert.equal(
    (withoutHtmlComments(TPL).match(/canAudit/g) ?? []).length,
    1,
    'canAudit 只许挂在对账面板上',
  );
  // 明细 / 汇总两个面板不许带条件（没有对账码的角色照样看数）
  assert.match(TPL, /<t-tab-panel :value="TRANSIT_TABS\[0\]\.key"/);
  assert.match(TPL, /<t-tab-panel :value="TRANSIT_TABS\[1\]\.key"/);
  assert.equal(TRANSIT_TABS.length, 2);
});

test('对账面板 lazy（首次切过去才跑全量扫描）', () => {
  assert.match(TPL, /:value="RECONCILE_TAB\.key"[^>]*lazy/);
  assert.match(PAGE_CODE, /watch\(activeTab[\s\S]{0,220}RECONCILE_TAB\.key/);
});

test('页面走 /reconcile 接口，且**不做任何客户端加总**（零 reduce）', () => {
  assert.match(PAGE_CODE, /fetchTransitReconcile/);
  assert.match(PAGE_CODE, /buildReconcileQuery/);
  assert.equal(/\.reduce\s*\(/.test(PAGE_CODE), false, '页面不许出现 reduce：合计一律取后端 totals');
});

test('页面不自己算差额 / 不把三侧数量相加（item_qty / received_qty / qty_in_transit 只读）', () => {
  for (const field of ['item_qty', 'received_qty', 'qty_in_transit', 'diff']) {
    assert.equal(
      new RegExp(`\\b${field}\\s*[-+*/]\\s`).test(PAGE_CODE),
      false,
      `页面在算 ${field}：三侧数量与差额只能取后端字段`,
    );
    assert.equal(
      new RegExp(`\\b${field}\\s*[-+*/]=`).test(PAGE_CODE),
      false,
      `页面在算 ${field}（复合赋值）`,
    );
  }
  assert.equal(/Number\([^)]*qty/.test(PAGE_CODE), false, '数量不许先转 Number 再算');
});

test('汇总 KPI 全部取 totals（checked / matched / mismatched / cancelled / untracked / 三侧 / diff）', () => {
  for (const testid of [
    'transit-reconcile-checked',
    'transit-reconcile-matched',
    'transit-reconcile-mismatched',
    'transit-reconcile-cancelled',
    'transit-reconcile-untracked',
    'transit-reconcile-total-item',
    'transit-reconcile-total-received',
    'transit-reconcile-total-transit',
    'transit-reconcile-total-diff',
  ]) {
    assert.ok(TPL.includes(testid), `KPI 缺 ${testid}`);
  }
  for (const field of [
    'totals.checked',
    'totals.matched',
    'totals.mismatched',
    'totals.cancelled',
    'totals.untracked',
    'totals.item_qty',
    'totals.received_qty',
    'totals.in_transit_qty',
    'totals.diff',
  ]) {
    assert.ok(TPL.includes(`reconcile.${field}`), `KPI 必须直接渲染后端字段 reconcile.${field}`);
  }
});

test('逐行差异列齐：发货单号（含日期）/ 物料 / 仓库 / 三侧数量 / 差异量 / 结论', () => {
  for (const colKey of [
    'ship_no',
    'material',
    'warehouse',
    'item_qty',
    'received_qty',
    'qty_in_transit',
    'diff',
    'issues',
  ]) {
    assert.match(TPL, new RegExp(`#${colKey}="\\{ row \\}"`), `缺单元格插槽 #${colKey}`);
  }
  // 发货日期并进发货单号的下行（独立列会挤掉「结论」列）
  assert.match(TPL, /#ship_no="\{ row \}"[\s\S]{0,1600}row\.ship_date/);
  // 结论文案取后端 issue_labels（issueText 只做拼接，不译字）；判定 tag 与文案同格
  assert.match(TPL, /#issues="\{ row \}"[\s\S]{0,400}reconcileResultText\(row\)/);
  assert.match(TPL, /#issues="\{ row \}"[\s\S]{0,900}issueText\(row\)/);
  assert.match(TPL, /issueTheme\(row\.issues\[0\]\)/);
});

test('空态与「全部对平」分开渲染（同一个空态插槽，文案由 reconcileEmptyText 分支给）', () => {
  assert.match(TPL, /data-testid="transit-reconcile-empty-title"/);
  assert.match(PAGE_CODE, /reconcileEmptyTextOf\(reconcile\.value, reconcileFilter\)/);
  assert.match(PAGE_CODE, /reconcileBannerOf\(reconcile\.value\)/);
  assert.match(TPL, /v-if="reconcileBanner"[\s\S]{0,200}transit-reconcile-banner/);
});

test('发货单号可点进那张单：带 ?shipment_id= 跳发货单页；没读码时退化成纯文本', () => {
  assert.match(PAGE_CODE, /hasPermission\('business:erp:shipment:list'\)/);
  assert.match(
    PAGE_CODE,
    /router\.push\(\{ path: '\/business\/erp\/shipment', query: \{ shipment_id: row\.shipment_id \} \}\)/,
  );
  assert.match(TPL, /v-if="canOpenShipment"[\s\S]{0,240}openShipment\(row\)/);
});

test('发货单页接了深链：onMounted 读 route.query.shipment_id → openDetailById', () => {
  const shipmentCode = withoutComments(SHIPMENT_PAGE);
  assert.match(shipmentCode, /const route = useRoute\(\)/);
  assert.match(shipmentCode, /route\.query\.shipment_id/);
  assert.match(shipmentCode, /await openDetailById\(target\)/);
  assert.match(shipmentCode, /async function openDetailById\(id: string\)/);
});

// ==================== ⑧ API 层契约 ====================

test('API：路径写全 /api + 对账路径，只读（无写接口）', () => {
  assert.match(API, /const BASE = '\/api\/business\/erp'/);
  assert.match(API, /fetchTransitReconcile[\s\S]{0,200}inventory-transit\/reconcile/);
  assert.match(API, /get<TransitReconcile>\(/);
  assert.equal(/\b(post|patch|put|del)\s*[<(]/.test(withoutComments(API)), false);
});

test('API：对账出参字段与后端 DTO 逐字一致（含 balanced / scan_limit / issue_labels）', () => {
  for (const field of [
    'shipment_deleted',
    'shipment_status',
    'item_qty',
    'shipped_qty',
    'received_qty',
    'qty_in_transit',
    'cancelled_qty',
    'diff',
    'issues',
    'issue_labels',
    'matched',
    'checked',
    'mismatched',
    'balanced',
    'truncated',
    'scan_limit',
    'untracked',
    'in_transit_qty',
  ]) {
    assert.ok(API.includes(field), `API 类型缺字段 ${field}`);
  }
  assert.ok(
    RECONCILE_RS.includes('pub const RECONCILE_SCAN_LIMIT'),
    '后端的扫描上限常量名变了：前端注释与文案要同步',
  );
});

/**
 * 采购明细页 / 单号序列页的纯逻辑单测（卡 t_6f023cee）。
 *
 * 覆盖的坑都是「看起来会算错、但编译期抓不到」的那几类：
 * - Decimal 出 JSON 是 `"1.5000"` 这种字符串 —— 直接渲染 4 位小数是噪音，
 *   但去掉末尾 0 时不能把整数位也吃掉（`"10.0000"` → `"10"`）；
 * - 税率量纲是 0–100（13% 存 `13.0000`），显示成 `13%` 而不是 `0.13%`；
 * - 「已收满」按数值比较（`"5.0000"` vs `"5"` 字符串比会误判），且 qty<=0 不算收满；
 * - 筛选空值一律不发（`?order_id=` 会让后端查出 0 行）；
 * - 单号序列的「下次序号」= current_no + step，step 脏数据时退化成 1 而不是 NaN。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  buildItemQuery,
  decimalText,
  emptyItemFilter,
  fallbackLabel,
  isFullyReceived,
  isItemFiltered,
  labelOf,
  receivedText,
  taxRateText,
} from '../../src/views/business/erp/purchase-order-item/purchase-order-item-logic.ts';
import {
  buildSeqQuery,
  datePatternText,
  emptySeqFilter,
  isSeqFiltered,
  nextNoOf,
  periodKeyText,
  seqStatusLabel,
  seqStatusTheme,
  separatorText,
} from '../../src/views/business/erp/seq/seq-logic.ts';

// ==================== 采购明细：数值显示 ====================

test('decimalText：去掉 Decimal 串的尾随 0，整数位保留', () => {
  assert.equal(decimalText('1.5000'), '1.5');
  assert.equal(decimalText('10.0000'), '10');
  assert.equal(decimalText('2.0000'), '2');
  assert.equal(decimalText('0.0000'), '0');
  assert.equal(decimalText('0.1234'), '0.1234');
  assert.equal(decimalText(3), '3');
  assert.equal(decimalText(1.25), '1.25');
});

test('decimalText：空值给「—」，不显示 0（区分「没填」与「填了 0」）', () => {
  assert.equal(decimalText(null), '—');
  assert.equal(decimalText(undefined), '—');
  assert.equal(decimalText(''), '—');
  assert.equal(decimalText('   '), '—');
});

test('taxRateText：量纲 0–100，0% 也照实显示', () => {
  assert.equal(taxRateText('13.0000'), '13%');
  assert.equal(taxRateText(6), '6%');
  assert.equal(taxRateText('0.0000'), '0%');
  assert.equal(taxRateText(null), '—');
});

test('receivedText：已收/应收，缺 received_qty 按 0', () => {
  assert.equal(receivedText({ received_qty: '2.0000', qty: '5.0000' }), '已收 2/5');
  assert.equal(receivedText({ received_qty: null, qty: '5' }), '已收 0/5');
});

test('isFullyReceived：按数值比较（"5.0000" 与 "5" 相等），qty<=0 不算收满', () => {
  assert.equal(isFullyReceived({ received_qty: '5.0000', qty: '5' }), true);
  assert.equal(isFullyReceived({ received_qty: '5.5', qty: '5' }), true, '超收也算收满');
  assert.equal(isFullyReceived({ received_qty: '4.9999', qty: '5' }), false);
  assert.equal(isFullyReceived({ received_qty: '0', qty: '0' }), false);
  assert.equal(isFullyReceived({ received_qty: 0, qty: -1 }), false);
});

// ==================== 采购明细：筛选与兜底 ====================

test('buildItemQuery：空筛选只发分页（不发 ?order_id=）', () => {
  assert.deepEqual(buildItemQuery(emptyItemFilter(), 2, 50), { page: 2, page_size: 50 });
  assert.equal(isItemFiltered(emptyItemFilter()), false);
});

test('buildItemQuery：两个 id 都给了就都发', () => {
  const query = buildItemQuery({ order_id: 'o-1', material_id: 'm-1', keyword: '' }, 1, 20);
  assert.deepEqual(query, { order_id: 'o-1', material_id: 'm-1', page: 1, page_size: 20 });
  assert.equal(isItemFiltered({ order_id: 'o-1', material_id: '', keyword: '' }), true);
});

test('buildItemQuery：keyword 去首尾空格后照发（后端 ILIKE 也 trim，两边一致）', () => {
  assert.deepEqual(buildItemQuery({ order_id: '', material_id: '', keyword: '  DEMO-P1 ' }, 1, 20), {
    keyword: 'DEMO-P1',
    page: 1,
    page_size: 20,
  });
});

test('buildItemQuery：空白 keyword 一律不发（不发 ?keyword= 空串，后端不会全表 ILIKE）', () => {
  for (const blank of ['', '   ', '\t\n']) {
    assert.deepEqual(
      buildItemQuery({ order_id: '', material_id: '', keyword: blank }, 1, 20),
      { page: 1, page_size: 20 },
      `keyword=${JSON.stringify(blank)} 必须当成「全部」`,
    );
    assert.equal(isItemFiltered({ order_id: '', material_id: '', keyword: blank }), false);
  }
});

test('buildItemQuery：keyword 与 id 条件同时发（后端是 AND）', () => {
  assert.deepEqual(buildItemQuery({ order_id: 'o-1', material_id: '', keyword: 'PO-2026' }, 3, 20), {
    order_id: 'o-1',
    keyword: 'PO-2026',
    page: 3,
    page_size: 20,
  });
  assert.equal(isItemFiltered({ order_id: '', material_id: '', keyword: '物料P1' }), true);
});

test('labelOf：主数据缺行时退回短 id，绝不留白', () => {
  const map = new Map([['m-1', '螺栓（M8）']]);
  assert.equal(labelOf(map, 'm-1'), '螺栓（M8）');
  assert.equal(labelOf(map, '3f2a9c1b-0000-4000-8000-000000000abc'), '3f2a9c1b…');
  assert.equal(labelOf(map, 'short'), 'short');
  assert.equal(labelOf(map, ''), '—');
  assert.equal(labelOf(new Map([['x', '   ']]), 'x'), 'x', '空白名字也退化成 id');
  assert.equal(fallbackLabel(null), '—');
});

// ==================== 单号序列 ====================

test('buildSeqQuery：keyword 去空格、状态 0（停用）也要发', () => {
  assert.deepEqual(buildSeqQuery(emptySeqFilter(), 1, 20), { page: 1, page_size: 20 });
  assert.deepEqual(buildSeqQuery({ keyword: '  PO  ', status: 0 }, 1, 20), {
    keyword: 'PO',
    status: 0,
    page: 1,
    page_size: 20,
  });
  assert.equal(isSeqFiltered(emptySeqFilter()), false);
  assert.equal(isSeqFiltered({ keyword: ' ', status: null }), false);
});

test('nextNoOf：current_no + step；step 非法时退化成 1', () => {
  assert.equal(nextNoOf({ current_no: 7, step: 1 }), 8);
  assert.equal(nextNoOf({ current_no: 0, step: 5 }), 5);
  assert.equal(nextNoOf({ current_no: 7, step: 0 }), 8);
  assert.equal(nextNoOf({ current_no: 7, step: Number.NaN }), 8);
});

test('seqStatus：1 启用（主色）/ 0 停用（灰）/ 未知原样', () => {
  assert.equal(seqStatusLabel(1), '启用');
  assert.equal(seqStatusLabel('0'), '停用');
  assert.equal(seqStatusLabel(null), '—');
  assert.equal(seqStatusLabel(9), '9');
  assert.equal(seqStatusTheme(1), 'primary');
  assert.equal(seqStatusTheme(0), 'default');
});

test('datePatternText / periodKeyText：三种周期口径 + 未取号', () => {
  assert.equal(datePatternText('yyyyMMdd'), '按日（yyyyMMdd）');
  assert.equal(datePatternText('yyyyMM'), '按月（yyyyMM）');
  assert.equal(datePatternText('none'), '不按周期');
  assert.equal(datePatternText(''), '不按周期');
  assert.equal(datePatternText('yyyy'), 'yyyy');

  assert.equal(periodKeyText({ period_key: '20261031', date_pattern: 'yyyyMMdd' }), '20261031');
  assert.equal(periodKeyText({ period_key: '', date_pattern: 'yyyyMMdd' }), '（尚未取号）');
  assert.equal(periodKeyText({ period_key: '', date_pattern: 'none' }), '—');
});

test('separatorText：空串显示「无」而不是留白', () => {
  assert.equal(separatorText('-'), '-');
  assert.equal(separatorText(''), '无');
  assert.equal(separatorText(null), '无');
});

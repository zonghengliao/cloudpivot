/**
 * VTRACE-4：单据详情页「查看凭证」取数口径（`shared/voucher-trace.ts`）单测。
 *
 * 运行：`pnpm test:unit`。
 *
 * 这里钉死的是**「六张单据各自该拿哪几个 (source_type, source_id) 去查」** ——
 * 也就是最容易写错、写错了还不报错（查出来永远是 0 张，按钮静默消失）的那一层：
 *
 * 1. 采购单 / 销售订单是**派生型**：必须顺着收货单 / 发货单的 `stock_out_id` 查，
 *    拿自己的 id 查永远 0 张；
 * 2. 报销单是**多来源型**：入账 + 付款两类都要查，只查一个漏一半；
 * 3. 空 id、未过账的发货单**不发查询**（后端会 400 / 白跑）；
 * 4. 合成清单按后端同一套排序键排，且按凭证 id 去重；
 * 5. 每条来源的 `type` 都在 `source-route.ts` 的值域内（否则后端 400+10001）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import type { VoucherRow } from '@/api/business/erp';
import { SOURCE_ROUTE_TABLE, isSourceDocType } from '@/views/business/erp/shared/source-route';
import {
  amountText,
  depreciationSources,
  expenseReportSources,
  hasRedVoucher,
  mergeVoucherRows,
  normalizeSources,
  purchaseOrderSources,
  salesOrderSources,
  sourceKey,
  sourceTypeLabel,
  sourcesKey,
  stockInSources,
  stockOutSources,
  voucherDocPathFor,
  voucherTraceButtonText,
  type VoucherSourceRef,
} from '@/views/business/erp/shared/voucher-trace';

/** 一个形状合法的 uuid（无业务含义）。 */
const ID_A = '018f3a52-0000-7000-8000-00000000000a';
const ID_B = '018f3a52-0000-7000-8000-00000000000b';

/** 造一张最小可用的凭证行（只填本文件会读到的列）。 */
function voucher(over: Partial<VoucherRow> & { id: string; voucher_no: string }): VoucherRow {
  return {
    voucher_word: '记',
    period: '2026-11',
    voucher_date: '2026-11-11',
    voucher_type: 'AUTO',
    voucher_type_label: '自动',
    source_type: 'STOCK_IN',
    source_id: ID_A,
    source_no: 'IN-20261111-000001',
    source_doc_no: 'IN-20261111-000001',
    summary: '采购入库',
    total_debit: '100.00',
    total_credit: '100.00',
    entry_count: 2,
    status: 'POSTED',
    status_label: '已记账',
    is_red: false,
    reverse_of: null,
    reversed_by: null,
    posted_at: '2026-11-11T09:00:00+08:00',
    posted_by: ID_B,
    remark: null,
    created_at: '2026-11-11T09:00:00+08:00',
    ...over,
  };
}

/** 造一行「待收明细里的收货单」（只填本文件会读到的列）。 */
function stockIn(over: Partial<{ id: string; in_no: string; posted: boolean }> = {}) {
  return {
    id: ID_A,
    in_no: 'IN-20261111-000001',
    status: 'POSTED',
    status_label: '已过账',
    in_date: '2026-11-11',
    total_qty: '10',
    posted: true,
    ...over,
  };
}

/** 造一行发货单（只填本文件会读到的列）。 */
function shipment(over: Partial<{ id: string; stock_out_id: string | null; stock_out_no: string | null }> = {}) {
  return {
    id: ID_B,
    ship_no: 'SH-20261111-000001',
    so_id: ID_A,
    so_no: 'SO-20261111-000001',
    customer_id: ID_A,
    customer_code: 'C001',
    customer_name: '客户',
    warehouse_id: ID_A,
    warehouse_code: 'W01',
    warehouse_name: '主仓',
    ship_date: '2026-11-11',
    status: 'POSTED',
    status_label: '已过账',
    total_qty: '5',
    stock_out_id: '018f3a52-0000-7000-8000-0000000000c1',
    stock_out_no: 'OUT-20261111-000001',
    remark: null,
    created_at: '2026-11-11T09:00:00+08:00',
    updated_at: '2026-11-11T09:00:00+08:00',
    items: [],
    ...over,
  } as unknown as Parameters<typeof salesOrderSources>[0][number];
}

// ==================== 六个取数口径 ====================

test('收货入库 / 出库：凭证挂本单，一条来源', () => {
  assert.deepEqual(stockInSources(ID_A, 'IN-20261111-000001'), [
    { type: 'STOCK_IN', id: ID_A, docNo: 'IN-20261111-000001', label: '收货入库单' },
  ]);
  assert.deepEqual(stockOutSources(ID_A, 'OUT-20261111-000001'), [
    { type: 'STOCK_OUT', id: ID_A, docNo: 'OUT-20261111-000001', label: '销售出库单' },
  ]);
});

test('收货入库 / 出库：id 为空 ⇒ 一条来源都不给（后端会 400，不发）', () => {
  assert.deepEqual(stockInSources('', 'IN-1'), []);
  assert.deepEqual(stockInSources(null), []);
  assert.deepEqual(stockInSources('   '), []);
  assert.deepEqual(stockOutSources(undefined, 'OUT-1'), []);
});

test('采购单（派生）：每条已生成的收货单 → 一条 STOCK_IN，跳过空 id', () => {
  const sources = purchaseOrderSources([
    stockIn({ id: ID_A, in_no: 'IN-20261111-000001' }),
    stockIn({ id: ID_B, in_no: 'IN-20261111-000002' }),
    stockIn({ id: '', in_no: 'IN-20261111-000003' }),
  ]);
  assert.equal(sources.length, 2);
  assert.deepEqual(
    sources.map((s) => [s.type, s.id, s.docNo]),
    [
      ['STOCK_IN', ID_A, 'IN-20261111-000001'],
      ['STOCK_IN', ID_B, 'IN-20261111-000002'],
    ],
  );
});

test('采购单：还没收货（无收货单）⇒ 没有来源 ⇒ 按钮不显示', () => {
  assert.deepEqual(purchaseOrderSources([]), []);
});

test('销售订单（派生）：未过账的发货单（stock_out_id=null）不发查询', () => {
  const sources = salesOrderSources([
    shipment({ id: ID_A, stock_out_id: '018f3a52-0000-7000-8000-0000000000c1', stock_out_no: 'OUT-1' }),
    shipment({ id: ID_B, stock_out_id: null, stock_out_no: null }),
  ]);
  assert.equal(sources.length, 1, '未过账的发货单没有出库单，也就没有凭证');
  assert.equal(sources[0].type, 'STOCK_OUT');
  assert.equal(sources[0].docNo, 'OUT-1');
});

test('报销单（多来源）：入账 + 付款两类，同一个 source_id', () => {
  const sources = expenseReportSources(ID_A, 'EXP-20261111-000001');
  assert.deepEqual(sources, [
    { type: 'EXPENSE_REPORT', id: ID_A, docNo: 'EXP-20261111-000001', label: '费用报销单（入账）' },
    { type: 'EXPENSE_PAYMENT', id: ID_A, docNo: 'EXP-20261111-000001', label: '费用报销单（付款）' },
  ]);
  assert.equal(sources[0].id, sources[1].id, '两类凭证挂的是同一张报销单');
  assert.notEqual(sources[0].type, sources[1].type, 'type 必须不同，否则漏查一半');
});

test('折旧批次：凭证挂本批次（DEPRECIATION）', () => {
  assert.deepEqual(depreciationSources(ID_A, 'DEP-202611-0001'), [
    { type: 'DEPRECIATION', id: ID_A, docNo: 'DEP-202611-0001', label: '折旧批次' },
  ]);
  assert.deepEqual(depreciationSources(null), []);
});

test('六个口径产出的 type 全部在 source-route 值域内（否则后端 400+10001）', () => {
  const all: VoucherSourceRef[] = [
    ...stockInSources(ID_A, 'IN-1'),
    ...stockOutSources(ID_A, 'OUT-1'),
    ...purchaseOrderSources([stockIn()]),
    ...salesOrderSources([shipment()]),
    ...expenseReportSources(ID_A, 'EXP-1'),
    ...depreciationSources(ID_A, 'DEP-1'),
  ];
  assert.ok(all.length >= 6);
  for (const source of all) {
    assert.ok(
      isSourceDocType(source.type),
      `${source.type} 不在 SOURCE_ROUTE_TABLE 里 —— 后端 VoucherSourceType 里没有这个取值`,
    );
    assert.ok(Object.hasOwn(SOURCE_ROUTE_TABLE, source.type));
    assert.ok(source.label.length > 0, '展示名不能为空');
  }
});

// ==================== 归集 / 去重 / 排序 ====================

test('normalizeSources：同 type 同 id 去重，不同 type 同 id 保留（报销单两类都要查）', () => {
  const list = normalizeSources([
    { type: 'EXPENSE_REPORT', id: ID_A, docNo: 'EXP-1', label: 'a' },
    { type: 'EXPENSE_REPORT', id: ID_A, docNo: 'EXP-1', label: 'a' },
    { type: 'EXPENSE_PAYMENT', id: ID_A, docNo: 'EXP-1', label: 'b' },
    { type: 'STOCK_IN', id: '  ', docNo: null, label: 'c' },
  ]);
  assert.equal(list.length, 2);
  assert.deepEqual(
    list.map((s) => s.type),
    ['EXPENSE_REPORT', 'EXPENSE_PAYMENT'],
  );
});

test('sourcesKey：与顺序无关，内容变了才变（watch 防抖的判据）', () => {
  const a: VoucherSourceRef = { type: 'STOCK_IN', id: ID_A, docNo: 'IN-1', label: 'x' };
  const b: VoucherSourceRef = { type: 'STOCK_OUT', id: ID_B, docNo: 'OUT-1', label: 'y' };
  assert.equal(sourcesKey([a, b]), sourcesKey([b, a]));
  assert.notEqual(sourcesKey([a]), sourcesKey([a, b]));
  assert.equal(sourcesKey([]), '');
});

test('sourceKey：type + id', () => {
  assert.equal(sourceKey({ type: 'STOCK_IN', id: ID_A, docNo: null, label: '' }), `STOCK_IN:${ID_A}`);
});

test('mergeVoucherRows：按凭证 id 去重（红冲后同来源两张都要留）', () => {
  const original = voucher({ id: ID_A, voucher_no: 'PZ-20261111-000001' });
  const red = voucher({ id: ID_B, voucher_no: 'PZR-20261111-000001', is_red: true, source_type: 'STOCK_IN' });
  const merged = mergeVoucherRows([[original], [original, red], null, undefined]);
  assert.equal(merged.length, 2);
  assert.deepEqual(
    merged.map((r) => r.voucher_no).sort(),
    ['PZ-20261111-000001', 'PZR-20261111-000001'],
  );
});

test('mergeVoucherRows：posted_at DESC、NULL 最后，再比 created_at / voucher_no', () => {
  const merged = mergeVoucherRows([
    [
      voucher({ id: 'a', voucher_no: 'PZ-3', posted_at: null, created_at: '2026-11-11T10:00:00+08:00' }),
      voucher({ id: 'b', voucher_no: 'PZ-1', posted_at: '2026-11-11T09:00:00+08:00' }),
      voucher({ id: 'c', voucher_no: 'PZ-2', posted_at: '2026-11-11T09:00:00+08:00', created_at: '2026-11-11T11:00:00+08:00' }),
    ],
  ]);
  assert.deepEqual(
    merged.map((r) => r.voucher_no),
    ['PZ-2', 'PZ-1', 'PZ-3'],
    '同一记账时间按 created_at 倒序；未记账（null）垫底',
  );
});

test('hasRedVoucher：红冲后为 true', () => {
  assert.equal(hasRedVoucher([voucher({ id: ID_A, voucher_no: 'PZ-1' })]), false);
  assert.equal(
    hasRedVoucher([voucher({ id: ID_A, voucher_no: 'PZ-1' }), voucher({ id: ID_B, voucher_no: 'PZR-1', is_red: true })]),
    true,
  );
});

// ==================== 展示 / 跳转 ====================

test('按钮文案：查看凭证 (N)', () => {
  assert.equal(voucherTraceButtonText(1), '查看凭证 (1)');
  assert.equal(voucherTraceButtonText(3), '查看凭证 (3)');
});

test('voucherDocPathFor：与应付页 voucherDocPath 同口径，单号要编码', () => {
  assert.equal(voucherDocPathFor('PZ-20261111-000001'), '/business/erp/voucher?voucher_no=PZ-20261111-000001');
  assert.equal(voucherDocPathFor(' PZ 1 '), '/business/erp/voucher?voucher_no=PZ%201');
  assert.equal(voucherDocPathFor(null), null);
  assert.equal(voucherDocPathFor(''), null);
});

test('voucherDocPathFor：目标页就是凭证页路由（防 404）', () => {
  const router = readFileSync(new URL('../../src/router/index.ts', import.meta.url), 'utf8');
  // 路由是**手工注册**的（没有 views 自动扫描），且父级已带 `/`，所以这里是不带前导斜杠的那一段
  assert.ok(router.includes("path: 'business/erp/voucher'"), '凭证页路由必须在 router/index.ts 里注册过');
});

test('sourceTypeLabel：已知类型给中文名，未知类型原样返回（不编造）', () => {
  assert.equal(sourceTypeLabel('STOCK_IN'), '收货入库单');
  assert.equal(sourceTypeLabel('EXPENSE_PAYMENT'), '费用报销单（付款）');
  assert.equal(sourceTypeLabel('NOT_A_TYPE'), 'NOT_A_TYPE');
  assert.equal(sourceTypeLabel(''), '—');
  assert.equal(sourceTypeLabel(null), '—');
});

test('amountText：借贷不平显示两侧，平则只显示一个', () => {
  assert.equal(amountText({ total_debit: '100.00', total_credit: '100.00' }), '100.00');
  assert.equal(amountText({ total_debit: '100.00', total_credit: '99.00' }), '100.00 / 99.00');
  assert.equal(amountText({ total_debit: '', total_credit: '' }), '—');
});

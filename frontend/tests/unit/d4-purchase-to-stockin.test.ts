/**
 * D4「采购订单 → 一键生成收货单」前端纯逻辑单测。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * 口径与后端对齐（`backend/src/business/erp/purchase_handler.rs::pending_items_of`）：
 * - 待收 = 订单 − 已收（**只算已过账收货单**）；
 * - 带出时只带 `pending_qty > 0` 的行，数量默认 = 待收数量；
 * - 保存时每行必须带回 `purchase_order_item_id`（后端按明细行回写已收数量）；
 * - 超收在前端先拦一次，文案要点明「本次最多可收 N（订单 Y / 已收 Z）」。
 *
 * 这些断言一旦漂移，`vue-tsc` 抓不到，e2e 也只看得见界面——所以钉在纯函数上。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  buildStockInPayload,
  buildStockInQuery,
  emptyStockInFilter,
  emptyStockInForm,
  formFromPending,
  isStockInFiltered,
  validateStockInForm,
  type StockInItemForm,
} from '@/views/business/erp/stockin/stockin-logic';

/** 一行待收明细（默认：订单 100 / 已收 30 / 待收 70） */
function pendingItem(overrides: Record<string, unknown> = {}) {
  return {
    item_id: 'po-item-1',
    material_id: 'mat-0001',
    ordered_qty: 100,
    received_qty: 30,
    pending_qty: 70,
    price_excl: 2,
    tax_rate: 13,
    ...overrides,
  };
}

/** 一份待收响应（两张明细：A 待收 70、B 已收满） */
function pendingRes() {
  return {
    order: {
      id: 'po-1',
      order_no: 'PO-20261101-000001',
      supplier_id: 'sup-1',
      warehouse_id: 'wh-1',
    },
    items: [
      pendingItem(),
      pendingItem({
        item_id: 'po-item-2',
        material_id: 'mat-0002',
        ordered_qty: 50,
        received_qty: 50,
        pending_qty: 0,
      }),
    ],
  };
}

/** 一行收货明细 */
function inLine(overrides: Partial<StockInItemForm> = {}): StockInItemForm {
  return { key: 'row-1', material_id: 'mat-0001', qty: 10, price_excl: 2, tax_rate: 13, ...overrides };
}

test('D4 · 带出：只带待收 > 0 的行，数量默认 = 待收数量', () => {
  const { form, lineCount } = formFromPending(pendingRes());
  assert.equal(lineCount, 1, '已收满的行不该带出来');
  assert.equal(form.items.length, 1);
  const [line] = form.items;
  assert.equal(line.material_id, 'mat-0001');
  assert.equal(line.qty, 70, '默认为待收 70，不是订单 100');
  assert.equal(line.price_excl, 2, '单价从采购带出');
  assert.equal(line.tax_rate, 13, '税率从采购带出');
  assert.deepEqual(line.pending, { ordered_qty: 100, received_qty: 30, pending_qty: 70 });
});

test('D4 · 带出：单头（供应商 / 仓库 / 来源单号）全部来自源单', () => {
  const { form } = formFromPending(pendingRes());
  assert.equal(form.purchase_order_id, 'po-1');
  assert.equal(form.purchase_order_no, 'PO-20261101-000001');
  assert.equal(form.supplier_id, 'sup-1');
  assert.equal(form.warehouse_id, 'wh-1');
  // 新建草稿，不带单据 id（走建单接口而不是改单）
  assert.equal(form.id, '');
});

test('D4 · 带出：NUMERIC 以字符串下发也能正确转数（"70.0000" → 70）', () => {
  const { form } = formFromPending({
    order: pendingRes().order,
    items: [
      pendingItem({
        ordered_qty: '100.0000',
        received_qty: '30.0000',
        pending_qty: '70.0000',
        price_excl: '2.500000',
      }),
    ],
  });
  assert.equal(form.items[0].qty, 70);
  assert.equal(form.items[0].price_excl, 2.5);
  assert.equal(form.items[0].pending?.pending_qty, 70);
});

test('D4 · 带出：一行待收都没有时留一个空行（界面不该是零行）', () => {
  const { form, lineCount } = formFromPending({
    order: pendingRes().order,
    items: [pendingItem({ pending_qty: 0, received_qty: 100 })],
  });
  assert.equal(lineCount, 0);
  assert.equal(form.items.length, 1);
  assert.equal(form.items[0].material_id, '', '占位行是空的，等用户自己加');
});

test('D4 · 保存：每行带回 purchase_order_item_id + 单头带 purchase_order_id', () => {
  const { form } = formFromPending(pendingRes());
  const payload = buildStockInPayload(form);
  assert.equal(payload.purchase_order_id, 'po-1', '要用卡面字段名 purchase_order_id');
  assert.equal(payload.source_order_id, undefined, '别名不能同时给（后端要求相等否则 400）');
  assert.equal(payload.items[0].purchase_order_item_id, 'po-item-1');
  assert.equal(payload.items[0].qty, 70);
});

test('D4 · 保存：手工建单（无来源）不带 purchase_order_id（给 null，不误挂单）', () => {
  const payload = buildStockInPayload(emptyStockInForm());
  assert.equal(payload.purchase_order_id, null);
  assert.equal(payload.items[0].purchase_order_item_id, null);
});

test('D4 · 防超收：数量超过待收 → 前端先拦，文案点明本次上限与三个数', () => {
  const line = inLine({
    qty: 80,
    pending: { ordered_qty: 100, received_qty: 30, pending_qty: 70 },
  });
  const form = { ...emptyStockInForm(), supplier_id: 'sup-1', warehouse_id: 'wh-1', items: [line] };
  const errors = validateStockInForm(form);
  const message = errors['items.0.qty'] ?? '';
  assert.match(message, /最多可收 70/, message);
  assert.match(message, /订单 100/, message);
  assert.match(message, /已收 30/, message);
});

test('D4 · 防超收：刚好等于待收（部分收货的极限）要放行', () => {
  const line = inLine({
    qty: 70,
    pending: { ordered_qty: 100, received_qty: 30, pending_qty: 70 },
  });
  // A6/A4：有「待收」的行必然带来源采购单，采样数据要跟着带上（否则先被「必须选采购单」拦掉）
  const form = {
    ...emptyStockInForm(),
    purchase_order_id: 'po-1',
    supplier_id: 'sup-1',
    warehouse_id: 'wh-1',
    items: [line],
  };
  assert.deepEqual(validateStockInForm(form), {});
});

test('D4 · 防超收：改小数量（部分收货是常态）不拦', () => {
  const line = inLine({
    qty: 30,
    pending: { ordered_qty: 100, received_qty: 30, pending_qty: 70 },
  });
  const form = {
    ...emptyStockInForm(),
    purchase_order_id: 'po-1',
    supplier_id: 'sup-1',
    warehouse_id: 'wh-1',
    items: [line],
  };
  assert.deepEqual(validateStockInForm(form), {});
});

test('D4/O3 · 筛选：按来源采购单号进查询参数，清空后不发', () => {
  const filter = { ...emptyStockInFilter(), purchase_order_no: ' PO-2026 ' };
  const query = buildStockInQuery(filter, 1);
  assert.equal(query.purchase_order_no, 'PO-2026', '要去空格后再发');

  const empty = buildStockInQuery(emptyStockInFilter(), 1);
  assert.equal(empty.purchase_order_no, undefined);
});

test('D4/O3 · 筛选：只填采购单号也算「处于筛选态」（决定空表文案）', () => {
  assert.equal(isStockInFiltered({ ...emptyStockInFilter(), purchase_order_no: 'PO-1' }), true);
  assert.equal(isStockInFiltered(emptyStockInFilter()), false);
  assert.equal(isStockInFiltered({ ...emptyStockInFilter(), purchase_order_no: '   ' }), false);
});

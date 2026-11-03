/**
 * `stock-pick.ts` + `mergeStockPicks` 单测（R-P1-5「调拨单从库存选物料」）。
 *
 * 运行：`pnpm test:unit`（`tsx --test "tests/unit/**\/*.test.ts"`，无 DOM、零新依赖）。
 *
 * # 为什么这些点必须钉住
 *
 * 「从库存选择」把三个接口的结果拼成一行行给作业员勾选，错一个字段就是**账错**：
 *
 * | 风险 | 钉法 |
 * |---|---|
 * | 可用量把锁定量算成可用（超发） | `available_qty = qty − locked_qty`，锁定量超过账面时如实为负 |
 * | `from_location_id` 取错 id（拿库位主数据 id 当库存行 id） | 断言等于 `InventoryLocationRow.id` |
 * | 成本价取错行（同仓同物料多行时拿到 0） | 汇总行均价，零成本行不让位 |
 * | 一物一码物料带入后没提示选序列号 | `serial_required` 透传 |
 * | 同物料同库位带两次变两行 | `mergeStockPicks` 累加 |
 * | 同物料不同库位被合并（丢库位） | `mergeStockPicks` 各占一行 |
 * | 新建表单默认空行没被复用（提交报「物料必填」） | 复用首行空行 |
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { buildPickerRows, toStockPicks } from '@/views/business/erp/shared/stock-pick';
import type { InventoryLocationRow, InventoryRow, MaterialRow } from '@/api/business/erp';
import { emptyTransferItem, mergeStockPicks, type TransferItemForm } from '@/views/business/erp/transfer/transfer-logic';

const MATERIAL_A = 'aaaaaaaa-0000-4000-8000-00000000000a';
const MATERIAL_B = 'bbbbbbbb-0000-4000-8000-00000000000b';
const WAREHOUSE = 'wwwwwwww-0000-4000-8000-00000000000w';
const LOCATION_1 = 'l1111111-0000-4000-8000-000000000011';
const LOCATION_2 = 'l2222222-0000-4000-8000-000000000022';
const INV_LOC_1 = 'i1111111-0000-4000-8000-000000000011';
const INV_LOC_2 = 'i2222222-0000-4000-8000-000000000022';

function locationRow(over: Partial<InventoryLocationRow>): InventoryLocationRow {
  return {
    id: INV_LOC_1,
    material_id: MATERIAL_A,
    warehouse_id: WAREHOUSE,
    location_id: LOCATION_1,
    qty: '10.0000',
    locked_qty: '0.0000',
    last_txn_at: null,
    created_at: '2026-11-01T00:00:00Z',
    updated_at: '2026-11-01T00:00:00Z',
    ...over,
  } as InventoryLocationRow;
}

function stockRow(over: Partial<InventoryRow>): InventoryRow {
  return {
    id: 's0000000-0000-4000-8000-000000000001',
    material_id: MATERIAL_A,
    warehouse_id: WAREHOUSE,
    qty: '10.0000',
    locked_qty: '0.0000',
    avg_cost: '16.950000',
    amount: '169.5000',
    last_txn_at: null,
    created_at: '2026-11-01T00:00:00Z',
    updated_at: '2026-11-01T00:00:00Z',
    ...over,
  } as InventoryRow;
}

function material(over: Partial<MaterialRow>): MaterialRow {
  return {
    id: MATERIAL_A,
    material_code: 'M-A',
    material_name: '物料 A',
    serial_required: false,
    ...over,
  } as MaterialRow;
}

// ==================== buildPickerRows ====================

test('可用量 = 账面数量 − 锁定量（锁定量不能算成可调拨）', () => {
  const rows = buildPickerRows([locationRow({ qty: '10.0000', locked_qty: '4.0000' })], [stockRow({})], [material({})]);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].available_qty, 6);
  // 默认带入数量 = 可用量（不是账面量）
  assert.equal(rows[0].pick_qty, 6);
});

test('锁定量超过账面（脏数据）时可用量如实为负，默认带入数量仍是 0', () => {
  const rows = buildPickerRows([locationRow({ qty: '2.0000', locked_qty: '5.0000' })], [], []);
  assert.equal(rows[0].available_qty, -3);
  assert.equal(rows[0].pick_qty, 0);
});

test('from_location_id 口径：inventory_location_id 取库位库存行 id，location_id 单独保留', () => {
  const rows = buildPickerRows([locationRow({})], [stockRow({})], [material({})]);
  assert.equal(rows[0].inventory_location_id, INV_LOC_1);
  assert.equal(rows[0].location_id, LOCATION_1);
  // 两者不是同一个 id（同名反着指的坑）
  assert.notEqual(rows[0].inventory_location_id, rows[0].location_id);
});

test('成本价取本仓该物料的汇总行均价，并标记 has_cost', () => {
  const rows = buildPickerRows(
    [locationRow({})],
    [stockRow({ avg_cost: '16.950000' })],
    [material({})],
  );
  assert.equal(rows[0].unit_cost, 16.95);
  assert.equal(rows[0].has_cost, true);
});

test('同物料多行库存里，零成本行不让位给有成本的行', () => {
  // 库位行先出现（avg_cost 为 0），汇总行在后面才有成本 —— 不能因为「先到先得」丢掉成本
  const rows = buildPickerRows(
    [locationRow({})],
    [stockRow({ id: 's1', avg_cost: '0' }), stockRow({ id: 's2', avg_cost: '8.500000' })],
    [material({})],
  );
  assert.equal(rows[0].unit_cost, 8.5);
  assert.equal(rows[0].has_cost, true);
});

test('没有任何成本价时 has_cost=false（界面显示「无成本价」而不是 ¥0.0000 骗人）', () => {
  const rows = buildPickerRows([locationRow({})], [stockRow({ avg_cost: '0' })], [material({})]);
  assert.equal(rows[0].has_cost, false);
  assert.equal(rows[0].unit_cost, 0);
});

test('一物一码标记从物料主数据透传（物料不在主数据里时不报错，按非一物一码）', () => {
  const rows = buildPickerRows(
    [
      locationRow({ id: INV_LOC_1, material_id: MATERIAL_A }),
      locationRow({ id: INV_LOC_2, material_id: MATERIAL_B, location_id: LOCATION_2 }),
    ],
    [],
    [material({ id: MATERIAL_A, serial_required: true })],
  );
  const a = rows.find((row) => row.material_id === MATERIAL_A);
  const b = rows.find((row) => row.material_id === MATERIAL_B);
  assert.equal(a?.serial_required, true);
  assert.equal(b?.serial_required, false);
});

test('排序：可用量降序（有货的在最上面），同量按物料编码稳定', () => {
  const rows = buildPickerRows(
    [
      locationRow({ id: INV_LOC_1, material_id: MATERIAL_B, qty: '1', locked_qty: '0' }),
      locationRow({ id: INV_LOC_2, material_id: MATERIAL_A, qty: '9', locked_qty: '0' }),
    ],
    [],
    [
      material({ id: MATERIAL_A, material_code: 'A-001' }),
      material({ id: MATERIAL_B, material_code: 'B-001' }),
    ],
  );
  assert.deepEqual(rows.map((row) => row.material_id), [MATERIAL_A, MATERIAL_B]);
});

// ==================== toStockPicks ====================

test('只带出「勾选了且数量 > 0」的行', () => {
  const rows = buildPickerRows(
    [
      locationRow({ id: INV_LOC_1, qty: '5' }),
      locationRow({ id: INV_LOC_2, location_id: LOCATION_2, qty: '0' }),
    ],
    [],
    [material({})],
  );
  const zero = rows.find((row) => row.inventory_location_id === INV_LOC_2);
  assert.equal(zero?.pick_qty, 0);
  const picks = toStockPicks(rows, [INV_LOC_1, INV_LOC_2]);
  assert.equal(picks.length, 1);
  assert.equal(picks[0].from_location_id, INV_LOC_1);
  assert.equal(picks[0].qty, 5);
});

test('数量被夹在可用量以内（手工改大也不允许超发）', () => {
  const rows = buildPickerRows([locationRow({ qty: '3', locked_qty: '0' })], [], []);
  rows[0].pick_qty = 99;
  const picks = toStockPicks(rows, [INV_LOC_1]);
  assert.equal(picks[0].qty, 3);
});

// ==================== mergeStockPicks ====================

function item(over: Partial<TransferItemForm>): TransferItemForm {
  return { ...emptyTransferItem('row-x'), ...over };
}

test('空表单（一行空行）带入 1 条：复用那行，不留空行', () => {
  const merged = mergeStockPicks(
    [emptyTransferItem('row-1')],
    [{ material_id: MATERIAL_A, from_location_id: INV_LOC_1, qty: 4, serial_required: true }],
    (index) => `row-${index + 1}`,
  );
  assert.equal(merged.length, 1);
  assert.equal(merged[0].key, 'row-1');
  assert.equal(merged[0].material_id, MATERIAL_A);
  assert.equal(merged[0].qty, 4);
  assert.equal(merged[0].from_location_id, INV_LOC_1);
  assert.equal(merged[0].serial_required, true);
});

test('同物料 + 同库位 → 累加数量（不出现两行一样的物料）', () => {
  const merged = mergeStockPicks(
    [item({ key: 'row-1', material_id: MATERIAL_A, qty: 2, from_location_id: INV_LOC_1 })],
    [{ material_id: MATERIAL_A, from_location_id: INV_LOC_1, qty: 3 }],
    (index) => `new-${index}`,
  );
  assert.equal(merged.length, 1);
  assert.equal(merged[0].qty, 5);
});

test('同物料不同库位 → 各占一行（库位是调出的真实来源，合并就丢了）', () => {
  const merged = mergeStockPicks(
    [item({ key: 'row-1', material_id: MATERIAL_A, qty: 2, from_location_id: INV_LOC_1 })],
    [{ material_id: MATERIAL_A, from_location_id: INV_LOC_2, qty: 3 }],
    (index) => `new-${index}`,
  );
  assert.equal(merged.length, 2);
  assert.deepEqual(
    merged.map((row) => row.from_location_id),
    [INV_LOC_1, INV_LOC_2],
  );
});

test('数量 <= 0 的选择被丢弃（不产生空明细行）', () => {
  const merged = mergeStockPicks(
    [item({ key: 'row-1', material_id: MATERIAL_A, qty: 1, from_location_id: null })],
    [{ material_id: MATERIAL_B, from_location_id: INV_LOC_2, qty: 0 }],
    (index) => `new-${index}`,
  );
  assert.equal(merged.length, 1);
  assert.equal(merged[0].material_id, MATERIAL_A);
});

test('不改入参（返回新数组、新行对象）', () => {
  const original = [emptyTransferItem('row-1')];
  const merged = mergeStockPicks(
    original,
    [{ material_id: MATERIAL_A, from_location_id: INV_LOC_1, qty: 2 }],
    (index) => `new-${index}`,
  );
  assert.equal(original[0].material_id, '');
  assert.notEqual(merged, original);
  assert.notEqual(merged[0], original[0]);
});

/**
 * `shipment-query.ts` 单测（SO-11 发货单页的纯逻辑）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 断言口径照后端抄：
 * - 状态域 / 标签 = `backend/src/business/erp/shipment_model.rs`（`STATUSES` / `status_label`）；
 * - 合法边 / 必填原因 = `backend/src/business/shared/state_machine.rs`
 *   （`SHIPMENT_EDGES` / `SHIPMENT_REASON_EDGES`）—— 这两张表直接读后端源码比对，
 *   前端自己抄错一条边，这里先红；
 * - 「只带未发完的行」= `shipment_handler::resolve_items` 的 ① 分支（`remaining > 0`）；
 * - 请求体 = `CreateShipmentReq` / `ShipmentItemReq`（没有物料字段、qty 可留空）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import type { ShippableItem } from '@/api/business/erp/shipment';
import {
  SHIPMENT_EDGES,
  SHIPMENT_ITEM_COLUMNS,
  SHIPMENT_MAX_LINES,
  SHIPMENT_PAGE_SIZE,
  SHIPMENT_REASON_EDGES,
  SHIPMENT_STATUSES,
  SHIPMENT_STATUS_LABELS,
  SHIPMENT_STATUS_OPTIONS,
  allowedTargets,
  buildCreatePayload,
  buildShipmentQuery,
  canCancel,
  canPost,
  canSubmit,
  canTransition,
  emptyListText,
  emptyShipmentFilter,
  emptyShipmentForm,
  isDateString,
  isFullShipment,
  isShipmentFiltered,
  isTerminal,
  linesFromShippable,
  postDialogText,
  qtyText,
  rowActions,
  shipmentStatusLabel,
  shipmentStatusTheme,
  statusNeedsReason,
  stockOutText,
  totalQtyOf,
  validateShipmentForm,
  type ShipmentFormDraft,
} from '@/views/business/erp/shipment/shipment-query';

/** 读后端源码（仓库根的 `backend/src/...`），断言「前端没抄错」用 */
function backendSource(relative: string): string {
  return readFileSync(new URL(`../../../backend/src/${relative}`, import.meta.url), 'utf8');
}

/** 一行可发明细（字段逐字照 `ShippableItemRes`） */
function shippable(overrides: Partial<ShippableItem> = {}): ShippableItem {
  return {
    so_item_id: 'd5050000-0000-4000-8000-000000000201',
    line_no: 1,
    material_id: 'd5050000-0000-4000-8000-000000000301',
    ordered: '10.0000',
    delivered: '0.0000',
    reserved: '0.0000',
    remaining: '10.0000',
    ...overrides,
  };
}

/** 一张可提交的表单（3 行里带 1 行，数量 = 未发量） */
function form(overrides: Partial<ShipmentFormDraft> = {}): ShipmentFormDraft {
  const base = emptyShipmentForm('2026-11-06');
  base.so_id = 'd5050000-0000-4000-8000-000000000101';
  base.warehouse_id = 'd5050000-0000-4000-8000-000000000401';
  base.items = linesFromShippable([shippable()]);
  return { ...base, ...overrides };
}

// ==================== 状态域 / 状态机 ====================

test('状态域 4 值且顺序与后端 STATUSES 一致', () => {
  assert.deepEqual([...SHIPMENT_STATUSES], ['DRAFT', 'PENDING_POST', 'POSTED', 'CANCELLED']);
  // 后端源码里逐字有这四个值（顺序也一致）
  const source = backendSource('business/erp/shipment_model.rs');
  const anchor = source.slice(source.indexOf('pub const STATUSES: [&str; 4] = ['));
  const block = anchor.slice(0, anchor.indexOf('];'));
  const order = [...block.matchAll(/STATUS_(DRAFT|PENDING_POST|POSTED|CANCELLED)/g)].map(
    (match) => match[1],
  );
  assert.deepEqual(order, ['DRAFT', 'PENDING_POST', 'POSTED', 'CANCELLED']);
});

test('状态中文名与后端 status_label 逐字一致', () => {
  assert.deepEqual(SHIPMENT_STATUS_LABELS, {
    DRAFT: '草稿',
    PENDING_POST: '待过账',
    POSTED: '已过账',
    CANCELLED: '已取消',
  });
  const source = backendSource('business/erp/shipment_model.rs');
  for (const [code, label] of Object.entries(SHIPMENT_STATUS_LABELS)) {
    assert.ok(
      source.includes(`STATUS_${code} => "${label}"`),
      `后端 status_label 里应有 ${code} => ${label}`,
    );
  }
});

test('4 条合法边逐条等于后端 SHIPMENT_EDGES', () => {
  const source = backendSource('business/shared/state_machine.rs');
  const anchor = source.slice(source.indexOf('pub const SHIPMENT_EDGES: [(&str, &str); 4] = ['));
  const block = anchor.slice(0, anchor.indexOf('];'));
  const edges = [...block.matchAll(/\("([A-Z_]+)", "([A-Z_]+)"\)/g)].map((match) => [
    match[1],
    match[2],
  ]);
  assert.deepEqual(edges, [
    ['DRAFT', 'PENDING_POST'],
    ['PENDING_POST', 'POSTED'],
    ['DRAFT', 'CANCELLED'],
    ['PENDING_POST', 'CANCELLED'],
  ]);
  assert.deepEqual(
    SHIPMENT_EDGES.map(([from, to]) => [from, to]),
    edges,
  );
});

test('必填原因只有 PENDING_POST → CANCELLED（等于后端 SHIPMENT_REASON_EDGES）', () => {
  const source = backendSource('business/shared/state_machine.rs');
  const anchor = source.slice(
    source.indexOf('pub const SHIPMENT_REASON_EDGES: [(&str, &str); 1] = ['),
  );
  const block = anchor.slice(0, anchor.indexOf('];'));
  const edges = [...block.matchAll(/\("([A-Z_]+)", "([A-Z_]+)"\)/g)].map((match) => [
    match[1],
    match[2],
  ]);
  assert.deepEqual(edges, [['PENDING_POST', 'CANCELLED']]);
  assert.deepEqual(
    SHIPMENT_REASON_EDGES.map(([from, to]) => [from, to]),
    edges,
  );

  assert.equal(statusNeedsReason('PENDING_POST', 'CANCELLED'), true);
  assert.equal(statusNeedsReason('DRAFT', 'CANCELLED'), false);
  assert.equal(statusNeedsReason('DRAFT', 'PENDING_POST'), false);
  assert.equal(statusNeedsReason('PENDING_POST', 'POSTED'), false);
});

test('非法边一律不认：DRAFT→POSTED、POSTED→CANCELLED、CANCELLED→*', () => {
  assert.equal(canTransition('DRAFT', 'POSTED'), false);
  assert.equal(canTransition('POSTED', 'CANCELLED'), false);
  assert.equal(canTransition('CANCELLED', 'DRAFT'), false);
  assert.equal(canTransition('PENDING_POST', 'DRAFT'), false);
  assert.equal(canTransition('DRAFT', 'PENDING_POST'), true);
  assert.equal(canTransition('PENDING_POST', 'POSTED'), true);
});

test('allowedTargets 按边表顺序给出可选去向', () => {
  assert.deepEqual(allowedTargets('DRAFT'), ['PENDING_POST', 'CANCELLED']);
  assert.deepEqual(allowedTargets('PENDING_POST'), ['POSTED', 'CANCELLED']);
  assert.deepEqual(allowedTargets('POSTED'), []);
  assert.deepEqual(allowedTargets('CANCELLED'), []);
  assert.deepEqual(allowedTargets(null), []);
});

test('按钮矩阵：DRAFT 只能提交/作废，PENDING_POST 只能过账/作废，终态无动作', () => {
  assert.deepEqual(rowActions({ status: 'DRAFT' }), { submit: true, post: false, cancel: true });
  assert.deepEqual(rowActions({ status: 'PENDING_POST' }), {
    submit: false,
    post: true,
    cancel: true,
  });
  assert.deepEqual(rowActions({ status: 'POSTED' }), { submit: false, post: false, cancel: false });
  assert.deepEqual(rowActions({ status: 'CANCELLED' }), {
    submit: false,
    post: false,
    cancel: false,
  });
  assert.deepEqual(rowActions({ status: null }), { submit: false, post: false, cancel: false });
  // 单独函数也钉一遍（模板里可能只用一个）
  assert.equal(canSubmit('DRAFT'), true);
  assert.equal(canPost('PENDING_POST'), true);
  assert.equal(canPost('DRAFT'), false, 'DRAFT 不能直接过账：边表里没有这条边');
  assert.equal(canCancel('PENDING_POST'), true);
  assert.equal(isTerminal('POSTED'), true);
  assert.equal(isTerminal('CANCELLED'), true);
  assert.equal(isTerminal('PENDING_POST'), false);
});

test('状态标签 / 配色：认不出的编码回落编码本身与 default', () => {
  assert.equal(shipmentStatusLabel('POSTED'), '已过账');
  assert.equal(shipmentStatusLabel('WHATEVER'), 'WHATEVER');
  assert.equal(shipmentStatusLabel(null), '—');
  assert.equal(shipmentStatusTheme('POSTED'), 'success');
  assert.equal(shipmentStatusTheme('CANCELLED'), 'danger');
  assert.equal(shipmentStatusTheme('WHATEVER'), 'default');
  assert.equal(shipmentStatusTheme(undefined), 'default');
});

test('状态下拉：5 项（全部 + 4 态），首项 = 全部（value 空串）', () => {
  assert.equal(SHIPMENT_STATUS_OPTIONS.length, 5);
  assert.deepEqual(SHIPMENT_STATUS_OPTIONS[0], { label: '全部状态', value: '' });
  assert.deepEqual(
    SHIPMENT_STATUS_OPTIONS.slice(1).map((item) => item.value),
    ['DRAFT', 'PENDING_POST', 'POSTED', 'CANCELLED'],
  );
});

// ==================== 列表查询 ====================

test('空筛选 ⇒ 请求只带分页（空串根本不发）', () => {
  const query = buildShipmentQuery(emptyShipmentFilter(), 1);
  assert.deepEqual(query, { page: 1, page_size: SHIPMENT_PAGE_SIZE });
  assert.equal(SHIPMENT_PAGE_SIZE, 20);
  assert.equal(isShipmentFiltered(emptyShipmentFilter()), false);
});

test('筛选参数：单号 trim、状态与客户精确带上', () => {
  const query = buildShipmentQuery(
    { ship_no: '  SH-20261106  ', status: 'PENDING_POST', customer_id: 'cus-1' },
    3,
    50,
  );
  assert.deepEqual(query, {
    ship_no: 'SH-20261106',
    status: 'PENDING_POST',
    customer_id: 'cus-1',
    page: 3,
    page_size: 50,
  });
  assert.equal(isShipmentFiltered({ ship_no: '  ', status: '', customer_id: '' }), false);
  assert.equal(isShipmentFiltered({ ship_no: '', status: 'POSTED', customer_id: '' }), true);
});

test('空态文案区分「没数据」与「筛出来没有」', () => {
  assert.ok(emptyListText(emptyShipmentFilter()).includes('还没有发货单'));
  assert.ok(
    emptyListText({ ship_no: 'SH-1', status: '', customer_id: '' }).includes('当前筛选条件下'),
  );
});

test('列表列与详情明细列的 colKey 与后端出参同名', () => {
  const keys = SHIPMENT_ITEM_COLUMNS.map((column) => column.colKey);
  for (const key of ['line_no', 'material_id', 'qty', 'location_id', 'serial_nos', 'remark']) {
    assert.ok(keys.includes(key), `明细列应有 ${key}`);
  }
});

// ==================== 从订单带出 ====================

test('只带 remaining > 0 的行，按订单行号升序，数量默认未发量', () => {
  const lines = linesFromShippable([
    shippable({ line_no: 2, so_item_id: 'row-2', remaining: '0.0000' }),
    shippable({ line_no: 3, so_item_id: 'row-3', ordered: '5.0000', delivered: '2.0000', remaining: '3.0000' }),
    shippable({ line_no: 1, so_item_id: 'row-1', remaining: '10.0000' }),
  ]);
  assert.equal(lines.length, 2);
  assert.deepEqual(
    lines.map((line) => line.so_item_id),
    ['row-1', 'row-3'],
  );
  assert.deepEqual(
    lines.map((line) => line.qty),
    ['10', '3'],
  );
  // 行 key 唯一（t-table row-key 用）
  assert.equal(new Set(lines.map((line) => line.key)).size, 2);
  // 未发量口径 = 服务端 remaining，前端不自己算 ordered - delivered
  assert.equal(lines[1]!.remaining, '3');
});

test('整张订单都发完了 ⇒ 带出 0 行（校验要拦住并给同一句口径）', () => {
  const lines = linesFromShippable([shippable({ remaining: '0.0000' })]);
  assert.equal(lines.length, 0);
  const error = validateShipmentForm(form({ items: [] }));
  assert.ok(error && error.includes('没有可发货的明细'));
});

test('qtyText 4 位定标：字符串数字、空值、非法值都归一', () => {
  assert.equal(qtyText('10.0000'), '10');
  assert.equal(qtyText(3.5), '3.5');
  assert.equal(qtyText('2.123456'), '2.1235');
  assert.equal(qtyText(null), '0');
  assert.equal(qtyText('abc'), '0');
  assert.equal(qtyText(''), '0');
});

test('合计数量按行累加（4 位定标后取整到 4 位）', () => {
  assert.equal(totalQtyOf([{ qty: '10' }, { qty: 2.5 }, { qty: '' }]), 12.5);
});

// ==================== 表单校验 ====================

test('必填项：订单 / 仓库 / 日期格式 / 明细非空', () => {
  assert.ok(validateShipmentForm(form({ so_id: '' }))?.includes('请选择销售订单'));
  assert.ok(validateShipmentForm(form({ warehouse_id: '' }))?.includes('请选择发货仓库'));
  assert.ok(validateShipmentForm(form({ ship_date: '2026-13-01' }))?.includes('yyyy-MM-dd'));
  assert.equal(validateShipmentForm(form()), null, '默认全发应通过前端闸');
});

test('数量校验：0 / 负数 / 超过未发量都要拦住', () => {
  const zero = form();
  zero.items![0]!.qty = 0;
  assert.ok(validateShipmentForm(zero)?.includes('必须大于 0'));

  const negative = form();
  negative.items![0]!.qty = -1;
  assert.ok(validateShipmentForm(negative)?.includes('必须大于 0'));

  const over = form();
  over.items![0]!.qty = '11';
  const error = validateShipmentForm(over);
  assert.ok(error?.includes('超过未发量'), `实际：${error}`);
});

test('明细行数上限与备注上限', () => {
  const many = form();
  many.items = Array.from({ length: SHIPMENT_MAX_LINES + 1 }, () => ({
    ...linesFromShippable([shippable()])[0]!,
  }));
  assert.ok(validateShipmentForm(many)?.includes('明细最多'));

  const longRemark = form({ remark: 'x'.repeat(501) });
  assert.ok(validateShipmentForm(longRemark)?.includes('备注不能超过'));
});

test('isDateString 拒掉不存在的日期', () => {
  assert.equal(isDateString('2026-02-28'), true);
  assert.equal(isDateString('2026-02-30'), false);
  assert.equal(isDateString('2026-13-01'), false);
  assert.equal(isDateString('2026/02/01'), false);
  assert.equal(isDateString(''), false);
});

// ==================== 请求体 ====================

test('建单请求体：不带物料、qty 归一为 4 位定标、空白备注与空库位不发', () => {
  const draft = form({ remark: '  首批发货  ' });
  draft.items![0]!.location_id = '';
  draft.items![0]!.remark = '   ';
  const payload = buildCreatePayload(draft);
  assert.deepEqual(payload, {
    so_id: 'd5050000-0000-4000-8000-000000000101',
    warehouse_id: 'd5050000-0000-4000-8000-000000000401',
    ship_date: '2026-11-06',
    remark: '首批发货',
    items: [{ so_item_id: 'd5050000-0000-4000-8000-000000000201', qty: '10' }],
  });
  // 类型契约：明细里**不许**有 material_id（后端从订单行带出）
  assert.equal('material_id' in payload.items[0]!, false);
});

test('数量留空 = 交给服务端「该行全部未发量」', () => {
  const draft = form();
  draft.items![0]!.qty = '';
  const payload = buildCreatePayload(draft);
  assert.equal('qty' in payload.items[0]!, false, '空数量不该发 qty 字段');
});

test('指定库位与行备注照发（库位是 biz_location.id）', () => {
  const draft = form();
  draft.items![0]!.location_id = 'd5050000-0000-4000-8000-000000000501';
  draft.items![0]!.remark = '整托发';
  const payload = buildCreatePayload(draft);
  assert.deepEqual(payload.items[0], {
    so_item_id: 'd5050000-0000-4000-8000-000000000201',
    qty: '10',
    location_id: 'd5050000-0000-4000-8000-000000000501',
    remark: '整托发',
  });
});

test('isFullShipment：数量全等于未发量才算「整单全发」', () => {
  assert.equal(isFullShipment(form()), true);
  const partial = form();
  partial.items![0]!.qty = '3';
  assert.equal(isFullShipment(partial), false);
});

// ==================== 详情 / 过账文案 ====================

test('过账弹窗正文点名单据与合计，并说清三个真实效果', () => {
  const text = postDialogText({ ship_no: 'SH-20261106-000001', total_qty: '10.0000' });
  assert.ok(text.includes('SH-20261106-000001'));
  assert.ok(text.includes('10'));
  assert.ok(text.includes('销售出库单'));
  assert.ok(text.includes('三本账'));
  assert.ok(text.includes('成本结转凭证'));
  assert.ok(text.includes('已发数量'));
  assert.ok(text.includes('不可撤销'), '过账不可撤销必须写在确认框里');
});

test('关联出库单号：过账后有号、未过账与出库单被删分开表达', () => {
  assert.equal(stockOutText({ status: 'POSTED', stock_out_no: 'OUT-20261106-000001' }), 'OUT-20261106-000001');
  assert.equal(stockOutText({ status: 'PENDING_POST', stock_out_no: null }), '未过账');
  assert.equal(stockOutText({ status: 'DRAFT', stock_out_no: null }), '未过账');
  assert.equal(stockOutText({ status: 'POSTED', stock_out_no: null }), '出库单已删除');
});

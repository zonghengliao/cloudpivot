/**
 * `sales-return-query.ts` 单测（SO-31 销售退货单页的纯逻辑）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 断言口径照后端抄：
 * - 状态域 / 标签 / 类型 = `backend/src/business/erp/sales_return_model.rs`
 *   （`STATUSES` / `status_label` / `RETURN_TYPES` / `return_type_label`）；
 * - 合法边 / 必填原因 = `backend/src/business/shared/state_machine.rs`
 *   （`SALES_RETURN_EDGES` / `SALES_RETURN_REASON_EDGES`）—— 两张表直接读后端源码比对，
 *   前端自己抄错一条边，这里先红；
 * - 「只带可退的行」= `sales_return_handler::resolve_items` 的带出分支（`remaining > 0`）；
 * - 金额公式 = `sales_return_model::line_amounts`（`amount = round(price×qty,4)`、
 *   `tax = round(amount×rate/100,4)`）；
 * - 请求体 = `CreateReturnReq` / `ReturnItemReq`（**没有** price / tax_rate / material_id）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import type { ReturnableItem } from '@/api/business/erp/sales-return';
import {
  RETURN_COLUMNS,
  RETURN_EDGES,
  RETURN_MAIN_CHAIN,
  RETURN_MAX_LINES,
  RETURN_PAGE_SIZE,
  RETURN_REASON_EDGES,
  RETURN_STATUSES,
  RETURN_STATUS_LABELS,
  RETURN_STATUS_OPTIONS,
  RETURN_TYPE_LABELS,
  RETURN_TYPES,
  allowedTargets,
  buildCreatePayload,
  buildReturnQuery,
  canTransition,
  emptyReturnFilter,
  emptyReturnForm,
  isDateString,
  isFullReturn,
  isReturnFiltered,
  isTerminal,
  lineAmounts,
  linesFromReturnable,
  mainChainReached,
  moneyText,
  postDialogText,
  postingLinks,
  qtyText,
  returnStatusLabel,
  returnStatusTheme,
  returnTypeLabel,
  rowActions,
  statusNeedsReason,
  totalsOf,
  validateReturnForm,
  type ReturnFormDraft,
} from '@/views/business/erp/sales-return/sales-return-query';

/** 读后端源码（仓库根的 `backend/src/...`），断言「前端没抄错」用 */
function backendSource(relative: string): string {
  return readFileSync(new URL(`../../../backend/src/${relative}`, import.meta.url), 'utf8');
}

/** 一行可退明细（字段逐字照 `ReturnableItemRes`） */
function returnable(overrides: Partial<ReturnableItem> = {}): ReturnableItem {
  return {
    so_item_id: 'd5050000-0000-4000-8000-000000000201',
    line_no: 1,
    material_id: 'd5050000-0000-4000-8000-000000000301',
    shipped: '10.0000',
    returned: '2.0000',
    remaining: '8.0000',
    price: '100.000000',
    tax_rate: '13.0000',
    ...overrides,
  };
}

/** 一张可提交的表单（1 行，数量 = 可退量） */
function form(overrides: Partial<ReturnFormDraft> = {}): ReturnFormDraft {
  const base = emptyReturnForm('2026-11-07');
  base.shipment_id = 'd5050000-0000-4000-8000-000000000101';
  base.return_type = 'QUALITY';
  base.warehouse_id = 'd5050000-0000-4000-8000-000000000401';
  base.items = linesFromReturnable([returnable()]);
  return { ...base, ...overrides };
}

// ==================== 状态域 / 状态机 ====================

test('状态域 6 值且顺序与后端 STATUSES 一致', () => {
  assert.deepEqual(
    [...RETURN_STATUSES],
    ['DRAFT', 'SUBMITTED', 'APPROVED', 'PENDING_POST', 'POSTED', 'CANCELLED'],
  );
  const source = backendSource('business/erp/sales_return_model.rs');
  const anchor = source.slice(source.indexOf('pub const STATUSES: [&str; 6] = ['));
  const block = anchor.slice(0, anchor.indexOf('];'));
  const order = [
    ...block.matchAll(/STATUS_(DRAFT|SUBMITTED|APPROVED|PENDING_POST|POSTED|CANCELLED)/g),
  ].map((match) => match[1]);
  assert.deepEqual(order, ['DRAFT', 'SUBMITTED', 'APPROVED', 'PENDING_POST', 'POSTED', 'CANCELLED']);
});

test('状态中文名与后端 status_label 逐字一致（含 CANCELLED = 已取消）', () => {
  assert.deepEqual(RETURN_STATUS_LABELS, {
    DRAFT: '草稿',
    SUBMITTED: '已提交',
    APPROVED: '已批准',
    PENDING_POST: '待过账',
    POSTED: '已过账',
    CANCELLED: '已取消',
  });
  const source = backendSource('business/erp/sales_return_model.rs');
  for (const [code, label] of Object.entries(RETURN_STATUS_LABELS)) {
    assert.ok(
      source.includes(`STATUS_${code} => "${label}"`),
      `后端 status_label 里应有 ${code} => ${label}`,
    );
  }
});

test('退货类型 3 值与中文名与后端逐字一致', () => {
  assert.deepEqual([...RETURN_TYPES], ['QUALITY', 'WRONG_ITEM', 'OTHER']);
  assert.deepEqual(RETURN_TYPE_LABELS, { QUALITY: '质量', WRONG_ITEM: '错发', OTHER: '其他' });
  const source = backendSource('business/erp/sales_return_model.rs');
  for (const [code, label] of Object.entries(RETURN_TYPE_LABELS)) {
    assert.ok(
      source.includes(`TYPE_${code} => "${label}"`),
      `后端 return_type_label 里应有 ${code} => ${label}`,
    );
  }
});

test('7 条合法边逐条等于后端 SALES_RETURN_EDGES', () => {
  const source = backendSource('business/shared/state_machine.rs');
  const anchor = source.slice(source.indexOf('pub const SALES_RETURN_EDGES: [(&str, &str); 7] = ['));
  const block = anchor.slice(0, anchor.indexOf('];'));
  const edges = [...block.matchAll(/\("([A-Z_]+)", "([A-Z_]+)"\)/g)].map((match) => [
    match[1],
    match[2],
  ]);
  assert.equal(edges.length, 7);
  assert.deepEqual(RETURN_EDGES.map(([from, to]) => [from, to]), edges);
});

test('2 条必填原因的边逐条等于后端 SALES_RETURN_REASON_EDGES', () => {
  const source = backendSource('business/shared/state_machine.rs');
  const anchor = source.slice(
    source.indexOf('pub const SALES_RETURN_REASON_EDGES: [(&str, &str); 2] ='),
  );
  const block = anchor.slice(0, anchor.indexOf('];'));
  const edges = [...block.matchAll(/\("([A-Z_]+)", "([A-Z_]+)"\)/g)].map((match) => [
    match[1],
    match[2],
  ]);
  assert.deepEqual(edges, [
    ['SUBMITTED', 'CANCELLED'],
    ['APPROVED', 'CANCELLED'],
  ]);
  assert.deepEqual(RETURN_REASON_EDGES.map(([from, to]) => [from, to]), edges);
});

test('canTransition / allowedTargets：单向链、不跳步、终态出不去、待过账不能作废', () => {
  assert.equal(canTransition('DRAFT', 'SUBMITTED'), true);
  assert.equal(canTransition('SUBMITTED', 'APPROVED'), true);
  assert.equal(canTransition('APPROVED', 'PENDING_POST'), true);
  assert.equal(canTransition('PENDING_POST', 'POSTED'), true);
  assert.deepEqual(allowedTargets('DRAFT'), ['SUBMITTED', 'CANCELLED']);
  assert.deepEqual(allowedTargets('SUBMITTED'), ['APPROVED', 'CANCELLED']);
  assert.deepEqual(allowedTargets('APPROVED'), ['PENDING_POST', 'CANCELLED']);
  // 跳步 / 回退
  assert.equal(canTransition('DRAFT', 'APPROVED'), false);
  assert.equal(canTransition('DRAFT', 'POSTED'), false);
  assert.equal(canTransition('SUBMITTED', 'DRAFT'), false);
  // 终态出不去
  assert.deepEqual(allowedTargets('POSTED'), []);
  assert.deepEqual(allowedTargets('CANCELLED'), []);
  assert.equal(isTerminal('POSTED'), true);
  assert.equal(isTerminal('CANCELLED'), true);
  assert.equal(isTerminal('PENDING_POST'), false);
  // ⚠️ 边表里没有 PENDING_POST → CANCELLED（卡在待过账只能过账后退货）
  assert.equal(canTransition('PENDING_POST', 'CANCELLED'), false);
});

test('按钮显隐矩阵：5 个动作各对应一条边', () => {
  assert.deepEqual(rowActions({ status: 'DRAFT' }), {
    submit: true,
    approve: false,
    pendingPost: false,
    post: false,
    cancel: true,
  });
  assert.deepEqual(rowActions({ status: 'SUBMITTED' }), {
    submit: false,
    approve: true,
    pendingPost: false,
    post: false,
    cancel: true,
  });
  assert.deepEqual(rowActions({ status: 'APPROVED' }), {
    submit: false,
    approve: false,
    pendingPost: true,
    post: false,
    cancel: true,
  });
  assert.deepEqual(rowActions({ status: 'PENDING_POST' }), {
    submit: false,
    approve: false,
    pendingPost: false,
    post: true,
    cancel: false,
  });
  for (const status of ['POSTED', 'CANCELLED', null]) {
    assert.deepEqual(rowActions({ status }), {
      submit: false,
      approve: false,
      pendingPost: false,
      post: false,
      cancel: false,
    });
  }
});

test('statusNeedsReason：只有 SUBMITTED / APPROVED 作废要原因', () => {
  assert.equal(statusNeedsReason('DRAFT', 'CANCELLED'), false);
  assert.equal(statusNeedsReason('SUBMITTED', 'CANCELLED'), true);
  assert.equal(statusNeedsReason('APPROVED', 'CANCELLED'), true);
  assert.equal(statusNeedsReason('PENDING_POST', 'CANCELLED'), false);
});

test('状态线：作废单只点亮草稿，主链顺序与后端一致', () => {
  assert.deepEqual(
    RETURN_MAIN_CHAIN.map((item) => item.value),
    ['DRAFT', 'SUBMITTED', 'APPROVED', 'PENDING_POST', 'POSTED'],
  );
  assert.equal(mainChainReached('PENDING_POST', 'APPROVED'), true);
  assert.equal(mainChainReached('PENDING_POST', 'POSTED'), false);
  assert.equal(mainChainReached('CANCELLED', 'DRAFT'), true);
  assert.equal(mainChainReached('CANCELLED', 'SUBMITTED'), false);
});

test('未知状态 / 类型回落编码本身，不显示空白', () => {
  assert.equal(returnStatusLabel(null), '—');
  assert.equal(returnStatusLabel('WHATEVER'), 'WHATEVER');
  assert.equal(returnStatusLabel('SUBMITTED'), '已提交');
  assert.equal(returnStatusTheme('POSTED'), 'success');
  assert.equal(returnStatusTheme('WHATEVER'), 'default');
  assert.equal(returnTypeLabel('QUALITY'), '质量');
  assert.equal(returnTypeLabel('QUALITY', '质量（后端）'), '质量（后端）');
  assert.equal(returnTypeLabel(null), '—');
});

// ==================== 列表查询 / 列 ====================

test('筛选 → 请求参数：空串根本不发，页大小默认 20', () => {
  const query = buildReturnQuery(emptyReturnFilter(), 2);
  assert.deepEqual(query, { page: 2, page_size: RETURN_PAGE_SIZE });
  assert.equal(RETURN_PAGE_SIZE, 20);

  const full = buildReturnQuery(
    { return_no: ' SR-2026 ', status: 'SUBMITTED', customer_id: 'c-1' },
    1,
    50,
  );
  assert.deepEqual(full, {
    return_no: 'SR-2026',
    status: 'SUBMITTED',
    customer_id: 'c-1',
    page: 1,
    page_size: 50,
  });
  assert.equal(isReturnFiltered(emptyReturnFilter()), false);
  assert.equal(isReturnFiltered({ ...emptyReturnFilter(), status: 'DRAFT' }), true);
});

test('列表列必须含卡面点名的六列（单号 / 客户 / 日期 / 类型 / 金额 / 状态）', () => {
  const keys = RETURN_COLUMNS.map((column) => String(column.colKey));
  for (const key of [
    'return_no',
    'customer_name',
    'return_date',
    'return_type',
    'total_with_tax',
    'status',
  ]) {
    assert.ok(keys.includes(key), `列表缺列 ${key}`);
  }
  assert.ok(keys.includes('op'), '缺操作列');
  // 状态下拉含全部 6 态 + 「全部」
  assert.equal(RETURN_STATUS_OPTIONS.length, 7);
  assert.equal(RETURN_STATUS_OPTIONS[0]!.value, '');
});

// ==================== 从发货单带出 / 金额 ====================

test('可退明细 → 表单行：只留 remaining > 0、按行号排序、数量默认可退量', () => {
  const lines = linesFromReturnable([
    returnable({ line_no: 2, so_item_id: 'so-2', remaining: '0.0000' }),
    returnable({ line_no: 3, so_item_id: 'so-3', remaining: '4.5000', price: '20.000000' }),
    returnable({ line_no: 1, so_item_id: 'so-1' }),
  ]);
  assert.equal(lines.length, 2, 'remaining = 0 的行不该带出');
  assert.deepEqual(
    lines.map((line) => line.so_item_id),
    ['so-1', 'so-3'],
  );
  assert.equal(lines[0]!.qty, '8');
  assert.equal(lines[1]!.qty, '4.5');
  assert.equal(lines[1]!.price, '20');
  assert.equal(lines[0]!.location_id, '');
});

test('金额拆分与服务端 line_amounts 同公式（amount = round(price×qty,4)，tax = round(amount×rate/100,4)）', () => {
  // 与 sales_return_model::tests 里 100 × 1.5000 × 13% = 19.5000 的口径一致
  assert.deepEqual(lineAmounts('1.5', '100', '13'), { amount: 150, tax: 19.5 });
  assert.deepEqual(lineAmounts('3', '33.3333', '0'), { amount: 99.9999, tax: 0 });
  assert.deepEqual(lineAmounts('', '', ''), { amount: 0, tax: 0 });
});

test('表单预估合计 = 逐行金额 + 税额之和', () => {
  const draft = form();
  draft.items = linesFromReturnable([
    returnable({ so_item_id: 'so-1', remaining: '2', price: '100', tax_rate: '13' }),
    returnable({ so_item_id: 'so-2', line_no: 2, remaining: '1', price: '50', tax_rate: '13' }),
  ]);
  const totals = totalsOf(draft.items);
  assert.equal(totals.amount, 250);
  assert.equal(totals.tax, 32.5);
  assert.equal(totals.withTax, 282.5);
});

test('qtyText：4 位定标 + 去掉无意义 0', () => {
  assert.equal(qtyText('8.0000'), '8');
  assert.equal(qtyText('4.5000'), '4.5');
  assert.equal(qtyText(1.23456), '1.2346');
  assert.equal(qtyText('abc'), '0');
});

// ==================== 表单校验 ====================

test('表单校验：缺发货单 / 类型 / 仓库 / 日期非法 / 无明细 都拦住', () => {
  assert.ok(validateReturnForm(form({ shipment_id: '' }))!.includes('原发货单'));
  assert.ok(validateReturnForm(form({ return_type: '' }))!.includes('退货类型'));
  assert.ok(validateReturnForm(form({ warehouse_id: '' }))!.includes('仓库'));
  assert.ok(validateReturnForm(form({ return_date: '2026-02-30' }))!.includes('yyyy-MM-dd'));
  assert.ok(validateReturnForm(form({ items: [] }))!.includes('没有可退的明细'));
  assert.equal(validateReturnForm(form()), null);
});

test('表单校验：数量必须 > 0 且不超过可退量', () => {
  const zero = form();
  zero.items[0]!.qty = '0';
  assert.ok(validateReturnForm(zero)!.includes('必须大于 0'));

  const over = form();
  over.items[0]!.qty = '9';
  const message = validateReturnForm(over)!;
  assert.ok(message.includes('超过可退量 8'));
  assert.ok(message.includes('400'));
});

test('表单校验：行数上限', () => {
  const many = form();
  many.items = linesFromReturnable(
    Array.from({ length: RETURN_MAX_LINES + 1 }, (_, index) =>
      returnable({ so_item_id: `so-${index}`, line_no: index + 1 }),
    ),
  );
  assert.ok(validateReturnForm(many)!.includes(`最多 ${RETURN_MAX_LINES} 行`));
});

test('isDateString：真日期才放行', () => {
  assert.equal(isDateString('2026-11-07'), true);
  assert.equal(isDateString('2026-02-29'), false);
  assert.equal(isDateString('2026-13-01'), false);
  assert.equal(isDateString('2026-1-7'), false);
  assert.equal(isDateString(''), false);
});

// ==================== 建单入参 ====================

test('建单入参：带发货单 + 类型 + 仓库 + 日期，明细只发 so_item_id / qty / 库位 / 备注', () => {
  const draft = form();
  draft.items[0]!.qty = '3';
  draft.items[0]!.location_id = 'loc-1';
  draft.items[0]!.remark = '外箱破损';
  draft.remark = '  客户退回  ';
  const payload = buildCreatePayload(draft);
  assert.deepEqual(payload, {
    shipment_id: 'd5050000-0000-4000-8000-000000000101',
    warehouse_id: 'd5050000-0000-4000-8000-000000000401',
    return_type: 'QUALITY',
    return_date: '2026-11-07',
    remark: '客户退回',
    items: [{ so_item_id: 'd5050000-0000-4000-8000-000000000201', qty: '3', location_id: 'loc-1', remark: '外箱破损' }],
  });
  // 价格 / 税率 / 物料一律不发（服务端从订单行取）
  const item = payload.items[0] as Record<string, unknown>;
  assert.equal('price' in item, false);
  assert.equal('tax_rate' in item, false);
  assert.equal('material_id' in item, false);
});

test('isFullReturn：数量全等于可退量才算「整单全退」', () => {
  assert.equal(isFullReturn(form()), true);
  const partial = form();
  partial.items[0]!.qty = '3';
  assert.equal(isFullReturn(partial), false);
});

// ==================== 详情 / 过账文案 ====================

test('过账弹窗正文点名单据与合计，并说清五个真实效果', () => {
  const text = postDialogText({ return_no: 'SR-20261107-000001', total_with_tax: '1921.0000' });
  assert.ok(text.includes('SR-20261107-000001'));
  assert.ok(text.includes('1,921.00'));
  assert.ok(text.includes('退货入库单'));
  assert.ok(text.includes('三本账'));
  assert.ok(text.includes('借 1405'));
  assert.ok(text.includes('冲减原应收'));
  assert.ok(text.includes('红字发票'));
  assert.ok(text.includes('不可撤销'), '过账不可撤销必须写在确认框里');
});

test('过账回链文案：已生成 / 已删除 / 未过账 三态分开说', () => {
  assert.deepEqual(
    postingLinks({
      status: 'POSTED',
      stock_in_id: 'si-1',
      receivable_id: 'ar-1',
      red_invoice_id: null,
    }),
    { stockIn: '已生成', receivable: '已冲减', redInvoice: '未开票（无红票）' },
  );
  assert.deepEqual(
    postingLinks({ status: 'POSTED', stock_in_id: null, receivable_id: null, red_invoice_id: null }),
    { stockIn: '入库单已删除', receivable: '未关联应收', redInvoice: '未开票（无红票）' },
  );
  assert.deepEqual(
    postingLinks({
      status: 'PENDING_POST',
      stock_in_id: null,
      receivable_id: null,
      red_invoice_id: null,
    }),
    { stockIn: '未过账', receivable: '未过账', redInvoice: '未过账' },
  );
});

test('金额显示：两位小数 + 千分位', () => {
  assert.equal(moneyText('1921.0000'), '1,921.00');
  assert.equal(moneyText(0), '0.00');
  assert.equal(moneyText(null), '0.00');
  assert.equal(moneyText('abc'), '0.00');
});

/**
 * `sales-order-query.ts` 单测（SO-07 销售订单页的纯逻辑）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 为什么测 `.ts` 不测 `.vue`：用例跑在没有 DOM 的 node 里。「字段有没有抄错」
 * 「哪些状态能提交 / 撤回 / 作废」「哪条边必须给原因」「提交的请求体带了什么」
 * 「金额预览与后端口径是否一致」全在 `sales-order-query.ts`；`.vue` 只渲染，
 * 这些一旦漂移 `vue-tsc` 抓不到（编译期两个都是 `string`）。
 *
 * 断言口径照后端抄：`backend/src/business/erp/sales_order_handler.rs`（入参校验 /
 * `SalesOrderRes`）、`sales_order_model.rs`（`line_amounts` / `Totals::of`）、
 * `shared/state_machine.rs`（`SALES_ORDER_EDGES` / `SALES_ORDER_BASE` 的 requires_reason）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { SalesOrder } from '@/api/business/erp';
import {
  SALES_ORDER_MAX_LINES,
  SALES_ORDER_PAGE_SIZE,
  SALES_ORDER_STATUS_OPTIONS,
  SALES_ORDER_STATUS_LABELS,
  buildCreatePayload,
  buildSalesOrderQuery,
  buildUpdatePayload,
  canAddLine,
  canCancel,
  canClose,
  canRemoveLine,
  canSubmit,
  canWithdraw,
  computeLine,
  computeTotals,
  emptyListText,
  emptySalesOrderFilter,
  emptySalesOrderForm,
  formFromDetail,
  isDateString,
  isEditable,
  isSalesOrderFiltered,
  newSalesOrderLine,
  rowActions,
  salesOrderStatusLabel,
  salesOrderStatusTheme,
  statusNeedsReason,
  toItemReq,
  validateSalesOrderForm,
} from '@/views/business/erp/sales-order/sales-order-query';

/** 一张真形态的销售订单（字段逐字照 `SalesOrderRes`） */
function order(overrides: Partial<SalesOrder> = {}): SalesOrder {
  return {
    id: 'd5050000-0000-4000-8000-000000000101',
    so_no: 'SO-20261106-000001',
    customer_id: 'd5050000-0000-4000-8000-00000000c001',
    customer_code: 'CUS-0001',
    customer_name: '北京示例科技有限公司',
    contract_id: null,
    contract_no: null,
    // SO-37：销售员（未指定 = null；佣金按它归属）
    salesperson_id: null,
    salesperson_name: null,
    order_date: '2026-11-06',
    delivery_date: null,
    status: 'DRAFT',
    status_label: '草稿',
    total_amount: '200.0000',
    total_tax: '26.0000',
    total_with_tax: '226.0000',
    remark: null,
    created_at: '2026-11-06T02:00:00Z',
    updated_at: '2026-11-06T02:00:00Z',
    items: [
      {
        id: 'd5050000-0000-4000-8000-000000000201',
        line_no: 1,
        material_id: 'd5050000-0000-4000-8000-00000000b001',
        qty: '2.0000',
        price: '100.0000',
        amount: '200.0000',
        tax_rate: '13.0000',
        tax_amount: '26.0000',
        delivered_qty: '0.0000',
        open_qty: '2.0000',
      },
    ],
    approval_instance_id: null,
    ...overrides,
  };
}

// ==================== 状态域 / 配色 ====================

test('status labels/themes cover the 7-value domain and fall back to the code', () => {
  assert.deepEqual(Object.keys(SALES_ORDER_STATUS_LABELS).sort(), [
    'APPROVED',
    'CANCELLED',
    'CLOSED',
    'DRAFT',
    'OUTBOUND',
    'PARTIAL_OUT',
    'SUBMITTED',
  ]);
  // 后端 status_label 中文与本地表逐字一致（漂移就会两边说法不同）
  assert.equal(salesOrderStatusLabel('SUBMITTED'), '审批中');
  // 认不出的编码回落编码本身（不显示空白）
  assert.equal(salesOrderStatusLabel('PARTIAL_SHIPPED'), 'PARTIAL_SHIPPED');
  assert.equal(salesOrderStatusLabel(null), '—');

  // 配色对齐采购单同一套语义：草稿灰 / 审批中品牌蓝 / 部分出库橙 / 出库绿 / 作废红
  assert.equal(salesOrderStatusTheme('DRAFT'), 'default');
  assert.equal(salesOrderStatusTheme('SUBMITTED'), 'primary');
  assert.equal(salesOrderStatusTheme('APPROVED'), 'primary');
  assert.equal(salesOrderStatusTheme('PARTIAL_OUT'), 'warning');
  assert.equal(salesOrderStatusTheme('OUTBOUND'), 'success');
  assert.equal(salesOrderStatusTheme('CLOSED'), 'success');
  assert.equal(salesOrderStatusTheme('CANCELLED'), 'danger');
  // 未知状态不猜颜色
  assert.equal(salesOrderStatusTheme('WHATEVER'), 'default');

  // 下拉：全部 + 7 个状态
  assert.equal(SALES_ORDER_STATUS_OPTIONS.length, 8);
  assert.equal(SALES_ORDER_STATUS_OPTIONS[0]?.value, '');
});

// ==================== 按钮显隐（11 条合法边） ====================

test('action visibility follows LEGAL_EDGES, not "my own draft"', () => {
  // DRAFT：可编辑 / 提交 / 作废，不可撤回 / 关闭
  assert.equal(isEditable('DRAFT'), true);
  assert.equal(canSubmit('DRAFT'), true);
  assert.equal(canWithdraw('DRAFT'), false);
  assert.equal(canCancel('DRAFT'), true);
  assert.equal(canClose('DRAFT'), false);

  // SUBMITTED：撤回 + 作废；**不许编辑**（后端 409）
  assert.equal(isEditable('SUBMITTED'), false);
  assert.equal(canSubmit('SUBMITTED'), false);
  assert.equal(canWithdraw('SUBMITTED'), true);
  assert.equal(canCancel('SUBMITTED'), true);
  assert.equal(canClose('SUBMITTED'), false);

  // APPROVED：关闭 + 作废（`APPROVED → DRAFT` 不是合法边，所以没有撤回）
  assert.equal(canWithdraw('APPROVED'), false);
  assert.equal(canClose('APPROVED'), true);
  assert.equal(canCancel('APPROVED'), true);

  // PARTIAL_OUT：只能关闭（`PARTIAL_OUT → CANCELLED` 不在边表里）
  assert.equal(canClose('PARTIAL_OUT'), true);
  assert.equal(canCancel('PARTIAL_OUT'), false);

  // 终态：一个都不给
  for (const status of ['OUTBOUND', 'CLOSED', 'CANCELLED'] as const) {
    const actions = rowActions({ status });
    assert.deepEqual(
      actions,
      { edit: false, submit: false, withdraw: false, cancel: false, close: false },
      `${status} 是终态，不该有任何操作`,
    );
  }

  // DRAFT 的全量形态（模板直接用 rowActions）
  assert.deepEqual(rowActions({ status: 'DRAFT' }), {
    edit: true,
    submit: true,
    withdraw: false,
    cancel: true,
    close: false,
  });
});

test('reason is required only on the edges whose BASE edge says so', () => {
  // SALES_ORDER_BASE 里 requires_reason = true 的四条
  assert.equal(statusNeedsReason('SUBMITTED', 'CANCELLED'), true);
  assert.equal(statusNeedsReason('APPROVED', 'CANCELLED'), true);
  assert.equal(statusNeedsReason('APPROVED', 'CLOSED'), true);
  assert.equal(statusNeedsReason('PARTIAL_OUT', 'CLOSED'), true);
  // 草稿作废不要原因（从没占用额度、没发货）
  assert.equal(statusNeedsReason('DRAFT', 'CANCELLED'), false);
});

// ==================== 列表查询 ====================

test('filter → query drops empty fields and trims the order no', () => {
  const empty = emptySalesOrderFilter();
  assert.equal(isSalesOrderFiltered(empty), false);
  assert.deepEqual(buildSalesOrderQuery(empty, 1), {
    page: 1,
    page_size: SALES_ORDER_PAGE_SIZE,
  });

  const query = buildSalesOrderQuery(
    {
      so_no: '  SO-20261106  ',
      status: 'SUBMITTED',
      customer_id: 'd5050000-0000-4000-8000-00000000c001',
    },
    3,
  );
  assert.deepEqual(query, {
    so_no: 'SO-20261106',
    status: 'SUBMITTED',
    customer_id: 'd5050000-0000-4000-8000-00000000c001',
    page: 3,
    page_size: SALES_ORDER_PAGE_SIZE,
  });

  // 只有空格的单号 = 不过滤（后端把 `?so_no=` 当「等于空串」，会查出 0 行）
  assert.equal(buildSalesOrderQuery({ so_no: '   ', status: '', customer_id: '' }, 1).so_no, undefined);
  assert.equal(isSalesOrderFiltered({ so_no: '  ', status: '', customer_id: '' }), false);
  assert.equal(isSalesOrderFiltered({ so_no: '', status: 'DRAFT', customer_id: '' }), true);
});

test('empty-state copy distinguishes "no data" from "nothing matched"', () => {
  assert.match(emptyListText(emptySalesOrderFilter()), /还没有销售订单/);
  assert.match(emptyListText({ so_no: 'SO-1', status: '', customer_id: '' }), /当前筛选条件/);
});

// ==================== 金额（预览口径贴后端） ====================

test('line amounts: amount = round(price × qty, 4), tax = round(amount × rate / 100, 4)', () => {
  // 后端单测同款算例：2 × 100 @13% → 200 / 26
  assert.deepEqual(computeLine('2', '100', '13'), {
    amount: 200,
    tax_amount: 26,
    with_tax: 226,
  });

  // 4 位定标：3 × 0.3333 = 0.9999
  assert.equal(computeLine(3, '0.3333', '0').amount, 0.9999);
  // 税额基于**定标后的金额**再算（先乘后舍入的顺序不能反）
  assert.equal(computeLine(3, '0.3333', '13').tax_amount, 0.13);
  // 0% / 100% 边界
  assert.equal(computeLine(1, '100', 0).tax_amount, 0);
  assert.equal(computeLine(1, '100', 100).tax_amount, 100);
  // 非法输入不抛异常，一律 0（校验负责报错）
  assert.deepEqual(computeLine('', '', ''), { amount: 0, tax_amount: 0, with_tax: 0 });
  assert.deepEqual(computeLine('abc', '10', '13'), { amount: 0, tax_amount: 0, with_tax: 0 });
  assert.deepEqual(computeLine(0, '10', '13'), { amount: 0, tax_amount: 0, with_tax: 0 });
  assert.deepEqual(computeLine(-1, '10', '13'), { amount: 0, tax_amount: 0, with_tax: 0 });
});

test('order totals sum per-line rounded amounts (Totals::of 同口径)', () => {
  const totals = computeTotals([
    { qty: 2, price: '100', tax_rate: '13' },
    { qty: 1, price: '0.0001', tax_rate: '13' },
  ]);
  // 行 1：200 + 26；行 2：0.0001 + 0.0000（round(0.0001×13/100,4) = 0.0000）
  assert.equal(totals.amount, 200.0001);
  assert.equal(totals.tax, 26);
  assert.equal(totals.with_tax, 226.0001);
  // 空明细 → 全 0（不 NaN）
  assert.deepEqual(computeTotals([]), { amount: 0, tax: 0, with_tax: 0 });
});

// ==================== 表单校验（贴 build_order / build_item） ====================

test('date strings must be real calendar days', () => {
  assert.equal(isDateString('2026-11-06'), true);
  assert.equal(isDateString('2024-02-29'), true, '闰年 2 月 29 日合法');
  assert.equal(isDateString('2026-02-29'), false, '平年 2 月 29 日非法');
  assert.equal(isDateString('2026-13-01'), false, '13 月非法');
  assert.equal(isDateString('2026-11-31'), false, '11 月 31 日非法');
  assert.equal(isDateString('2026-1-1'), false, '必须补零');
  assert.equal(isDateString(''), false);
  assert.equal(isDateString(null), false);
});

test('form validation rejects every case the backend rejects', () => {
  const base = emptySalesOrderForm('2026-11-06');
  base.customer_id = 'd5050000-0000-4000-8000-00000000c001';
  base.items = [newSalesOrderLine({ material_id: 'd5050000-0000-4000-8000-00000000b001' })];

  assert.equal(validateSalesOrderForm(base), null, '合法表单应通过');

  // ① 客户必填
  assert.match(validateSalesOrderForm({ ...base, customer_id: '' }) ?? '', /请选择客户/);

  // ② 日期格式
  assert.match(validateSalesOrderForm({ ...base, order_date: '2026-13-01' }) ?? '', /yyyy-MM-dd/);

  // ③ 交期早于下单日（后端 build_order 同款）
  assert.match(
    validateSalesOrderForm({ ...base, delivery_date: '2026-11-05' }) ?? '',
    /不能早于下单日期/,
  );
  assert.equal(validateSalesOrderForm({ ...base, delivery_date: '2026-11-06' }), null);

  // ④ 0 行明细
  assert.match(validateSalesOrderForm({ ...base, items: [] }) ?? '', /至少要有 1 行明细/);

  // ⑤ 物料必填 / 数量 > 0 / 单价非负 / 税率 0–100
  const noMaterial = { ...base, items: [newSalesOrderLine({ qty: '1' })] };
  assert.match(validateSalesOrderForm(noMaterial) ?? '', /请选择物料/);

  for (const qty of ['0', '-1', 'abc']) {
    const bad = {
      ...base,
      items: [newSalesOrderLine({ material_id: 'm', qty })],
    };
    assert.match(validateSalesOrderForm(bad) ?? '', /数量必须大于 0/, `qty=${qty}`);
  }

  const badPrice = { ...base, items: [newSalesOrderLine({ material_id: 'm', price: '-0.0001' })] };
  assert.match(validateSalesOrderForm(badPrice) ?? '', /单价不能为负/);

  for (const rate of ['-1', '100.0001', '101']) {
    const bad = { ...base, items: [newSalesOrderLine({ material_id: 'm', tax_rate: rate })] };
    assert.match(validateSalesOrderForm(bad) ?? '', /税率必须在 0–100/, `rate=${rate}`);
  }
  // 0 / 100 是合法边界
  for (const rate of ['0', '100']) {
    assert.equal(
      validateSalesOrderForm({
        ...base,
        items: [newSalesOrderLine({ material_id: 'm', tax_rate: rate })],
      }),
      null,
      `rate=${rate} 应合法`,
    );
  }

  // ⑥ 备注超长
  assert.match(
    validateSalesOrderForm({ ...base, remark: 'x'.repeat(501) }) ?? '',
    /备注不能超过 500 字/,
  );
});

test('form validation reports which line failed', () => {
  const base = emptySalesOrderForm('2026-11-06');
  base.customer_id = 'c';
  base.items = [
    newSalesOrderLine({ material_id: 'm1', qty: '1' }),
    newSalesOrderLine({ material_id: 'm2', qty: '0' }),
  ];
  assert.match(validateSalesOrderForm(base) ?? '', /第 2 行：数量必须大于 0/);
});

test('line add/remove guards keep at least one line', () => {
  const form = emptySalesOrderForm('2026-11-06');
  assert.equal(form.items.length, 1);
  assert.equal(canRemoveLine(form), false, '只剩一行不许删（后端要求 ≥1 行）');
  assert.equal(canAddLine(form), true);

  form.items.push(newSalesOrderLine());
  assert.equal(canRemoveLine(form), true);

  form.items = Array.from({ length: SALES_ORDER_MAX_LINES }, () => newSalesOrderLine());
  assert.equal(canAddLine(form), false, `到 ${SALES_ORDER_MAX_LINES} 行就别让加了`);
});

// ==================== 请求体 ====================

test('item request carries only qty / price / tax_rate (amounts are server-side)', () => {
  const line = newSalesOrderLine({ material_id: 'm1', qty: '2', price: '100', tax_rate: '13' });
  assert.deepEqual(toItemReq(line), {
    material_id: 'm1',
    qty: '2',
    price: '100',
    tax_rate: '13',
  });
  // 空单价 / 空税率不发（后端 `unwrap_or(ZERO)` 同口径）
  const bare = toItemReq(newSalesOrderLine({ material_id: 'm1', qty: '1', price: '', tax_rate: '' }));
  assert.deepEqual(bare, { material_id: 'm1', qty: '1' });
});

test('create payload keeps dates and folds empty remark to null', () => {
  const form = emptySalesOrderForm('2026-11-06');
  form.customer_id = 'c1';
  form.delivery_date = '';
  form.remark = '   ';
  form.items = [newSalesOrderLine({ material_id: 'm1', qty: '2', price: '100', tax_rate: '13' })];

  assert.deepEqual(buildCreatePayload(form), {
    customer_id: 'c1',
    // 没选合同 → `null`（SO-27：`contract_id` 显式发 null，后端 serde 折成 None）
    contract_id: null,
    // 没选销售员 → `null`（SO-37：后端不默认成建单人 —— 佣金归属必须显式指定）
    salesperson_id: null,
    order_date: '2026-11-06',
    delivery_date: null,
    remark: null,
    items: [{ material_id: 'm1', qty: '2', price: '100', tax_rate: '13' }],
  });
});

test('SO-37 销售员：建单发 id、改单用 "" 解绑、详情 null 折成空串', () => {
  // ① 建单：选了销售员就发 id
  const createForm = emptySalesOrderForm('2026-11-07');
  createForm.customer_id = 'c1';
  createForm.salesperson_id = 'u-1';
  assert.equal(buildCreatePayload(createForm).salesperson_id, 'u-1');

  // ② 改单：必须显式发（省略 = 后端「不改」，用户就永远解不掉销售员）
  const updateForm = emptySalesOrderForm('2026-11-07');
  updateForm.customer_id = 'c1';
  updateForm.salesperson_id = '';
  assert.equal(buildUpdatePayload(updateForm).salesperson_id, '', '空串 = 解绑销售员');

  // ③ 详情 → 表单：`null`（老单 / 没指定）折成空串，别让 t-select 收到 null
  const withSalesperson = order({ salesperson_id: 'u-9', salesperson_name: '张三' });
  assert.equal(formFromDetail(withSalesperson).salesperson_id, 'u-9');
  const withoutSalesperson = order({ salesperson_id: null, salesperson_name: null });
  assert.equal(formFromDetail(withoutSalesperson).salesperson_id, '');
});

test('SO-27 合同：建单带上 contract_id，改单用 "" 表示解绑，详情回填', () => {
  // ① 建单：选了合同就发 id（空 = null，后端 serde 折成 None）
  const createForm = emptySalesOrderForm('2026-11-07');
  createForm.customer_id = 'c1';
  createForm.contract_id = 'ct-1';
  const create = buildCreatePayload(createForm);
  assert.equal(create.contract_id, 'ct-1');

  // ② 改单：必须显式发（省略 = 后端「不改」，用户就永远解不掉合同）
  const updateForm = emptySalesOrderForm('2026-11-07');
  updateForm.customer_id = 'c1';
  updateForm.contract_id = '';
  assert.equal(buildUpdatePayload(updateForm).contract_id, '', '空串 = 解绑合同');

  // ③ 详情 → 表单：挂过合同的单要能回填出合同（`null` → ''）
  const withContract = order({ contract_id: 'ct-9' });
  assert.equal(formFromDetail(withContract).contract_id, 'ct-9');
  const withoutContract = order({ contract_id: null, contract_no: null });
  assert.equal(formFromDetail(withoutContract).contract_id, '');
});

test('update payload uses "" for cleared delivery date / remark (整单覆盖)', () => {
  const form = emptySalesOrderForm('2026-11-07');
  form.customer_id = 'c1';
  form.delivery_date = '';
  form.remark = '';
  const payload = buildUpdatePayload(form);
  assert.equal(payload.delivery_date, '');
  assert.equal(payload.remark, '');
  // 明细整单传（后端全量替换）
  assert.equal(payload.items?.length, form.items.length);
});

test('detail → form round-trips the line values sorted by line_no', () => {
  const detail = order({
    delivery_date: '2026-11-10',
    remark: '客户要求分两批发',
    items: [
      {
        id: 'i2',
        line_no: 2,
        material_id: 'm2',
        qty: '3.0000',
        price: '12.5000',
        amount: '37.5000',
        tax_rate: '13.0000',
        tax_amount: '4.8750',
        delivered_qty: '0.0000',
        open_qty: '3.0000',
      },
      {
        id: 'i1',
        line_no: 1,
        material_id: 'm1',
        qty: '2.0000',
        price: '100.0000',
        amount: '200.0000',
        tax_rate: '13.0000',
        tax_amount: '26.0000',
        delivered_qty: '1.0000',
        open_qty: '1.0000',
      },
    ],
  });
  const form = formFromDetail(detail);
  assert.equal(form.customer_id, detail.customer_id);
  assert.equal(form.order_date, '2026-11-06');
  assert.equal(form.delivery_date, '2026-11-10');
  assert.equal(form.remark, '客户要求分两批发');
  // 行按 line_no 升序；尾零被裁掉（"2.0000" → "2"）
  assert.deepEqual(
    form.items.map((line) => [line.material_id, String(line.qty), String(line.price)]),
    [
      ['m1', '2', '100'],
      ['m2', '3', '12.5'],
    ],
  );
  // 回填后合计与后端返回的总额一致（前端预览不吃亏）
  const totals = computeTotals(form.items);
  assert.equal(totals.with_tax, 226 + 37.5 + 4.875);
});

test('null delivery date / remark in detail become empty strings in the form', () => {
  const form = formFromDetail(order({ delivery_date: null, remark: null }));
  assert.equal(form.delivery_date, '');
  assert.equal(form.remark, '');
});

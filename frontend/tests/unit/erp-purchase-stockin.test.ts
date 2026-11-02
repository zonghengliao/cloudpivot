/**
 * 采购订单页 + 收货入库页纯逻辑单测（P2-E1）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * 为什么测 `.ts` 不测 `.vue`：用例跑在没有 DOM 的 node 里，「100×2, 13% 算成 226 还是 200」
 * 「过账按钮该不该出现」「发票未到能不能选」全在 `*-logic.ts`；`.vue` 只负责渲染。
 * 这些一旦漂移，`vue-tsc` 抓不到，e2e 也只看得见「界面长什么样」。
 *
 * 断言口径照后端抄：
 * - 金额公式 = `backend/src/business/erp/purchase_model.rs::line_amounts`（口径 A / B + 减法倒挤）；
 * - 合计 = `sum_lines`（**逐行舍入后相加**）；
 * - 状态机 = `purchase_model::LEGAL_EDGES` / `stockin_model` 的三条边；
 * - 过账弹窗 = PRD §3.3.2（「发票已随货到达」默认勾选、「发票未到」置灰）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  AMOUNT_COLUMN_TITLE,
  availablePurchaseActions,
  buildPurchasePayload,
  buildStatusPayload,
  computeTotals,
  emptyPurchaseForm,
  formatMoney,
  lineAmounts,
  priceColumnTitle,
  priceModeLabel,
  switchConfirmMessage,
  switchPriceMode,
  toNumber,
  validatePurchaseForm,
  type PurchaseItemForm,
} from '@/views/business/erp/purchase/purchase-logic';
import {
  ESTIMATE_DISABLED_TIP,
  INVOICE_OPTIONS,
  availableStockInActions,
  buildCancelPayload,
  buildStockInPayload,
  buildPostPayload,
  computeTotals as computeStockInTotals,
  defaultInvoiceChoice,
  emptyStockInForm,
  formatMoney as formatStockInMoney,
  invoiceReceivedOf,
  validateStockInForm,
  type StockInItemForm,
} from '@/views/business/erp/stockin/stockin-logic';

/** 一行采购明细（各用例只改自己关心的字段） */
function line(overrides: Partial<PurchaseItemForm> = {}): PurchaseItemForm {
  return { key: 'row-1', material_id: 'mat-0001', qty: 100, price: 2, tax_rate: 13, ...overrides };
}

/** 一行收货明细 */
function inLine(overrides: Partial<StockInItemForm> = {}): StockInItemForm {
  return { key: 'row-1', material_id: 'mat-0001', qty: 100, price_excl: 2, tax_rate: 13, ...overrides };
}

// ==================== ① 口径 A（不含税录入） ====================

test('① 口径 A：qty=100 × 不含税单价 2，税率 13% → 价款 200 / 税额 26 / 价税合计 226', () => {
  const amounts = lineAmounts('EXCL', 100, 2, 13);
  assert.equal(amounts.amount, 200, '不含税金额 = round2(100 × 2)');
  assert.equal(amounts.tax, 26, '税额 = round2(200 × 13%)');
  assert.equal(amounts.total, 226, '价税合计 = 价款 + 税额');

  // 整单合计走的是同一个函数：逐行舍入后相加（PRD §3.1.5）
  const totals = computeTotals('EXCL', [line()]);
  assert.equal(totals.qty, 100);
  assert.equal(totals.amount, 200);
  assert.equal(totals.tax, 26);
  assert.equal(totals.total, 226);
  assert.equal(formatMoney(totals.total), '226.00', '弹窗底部显示 226.00');

  // 金额列标题必须带「(价税合计)」（PRD 硬要求）
  assert.equal(AMOUNT_COLUMN_TITLE, '金额(价税合计)');
  // 不含税口径下单价列叫「不含税单价」
  assert.equal(priceColumnTitle('EXCL'), '不含税单价');
  assert.equal(priceModeLabel('EXCL'), '不含税');
});

// ==================== ② 口径 B（含税录入） ====================

test('② 口径 B：qty=100 × 含税单价 2，税率 13% → 价款 176.99 / 税额 23.01 / 价税合计 200', () => {
  const amounts = lineAmounts('INCL', 100, 2, 13);
  assert.equal(amounts.total, 200, '价税合计 = round2(100 × 2)');
  assert.equal(amounts.amount, 176.99, '不含税 = round2(200 / 1.13)');
  // ★ 减法倒挤：再乘一次税率会得到 23.00，与 200 − 176.99 = 23.01 差 1 分，
  //   而库里 ck_biz_po_item_tax 是精确恒等，写不进去
  assert.equal(amounts.tax, 23.01, '税额 = 价税合计 − 不含税（减法倒挤）');
  assert.equal(
    Number((amounts.amount + amounts.tax).toFixed(2)),
    amounts.total,
    'amount + tax === with_tax（库 CHECK 精确恒等）',
  );
  assert.equal(formatMoney(amounts.total), '200.00');
  assert.equal(priceColumnTitle('INCL'), '含税单价');
  assert.equal(priceModeLabel('INCL'), '含税');
});

// ==================== ③ 口径切换 ====================

test('③ 口径切换：同一串录入数字被重新解释，各行金额全部重算（226.00 → 200.00）', () => {
  const items = [line()];

  const before = computeTotals('EXCL', items);
  assert.equal(before.total, 226, '不含税口径：价税合计 226.00');

  // 确认文案是 PRD §3.1.4 的原话
  assert.equal(
    switchConfirmMessage('INCL'),
    '切换为含税录入后，所有行将按各行税率重新计算金额，是否继续？',
  );

  // 切换：保留录入的单价数值，只换口径（反之则「合计金额」会整单漂移）
  const mode = switchPriceMode(items, 'INCL');
  assert.equal(mode, 'INCL');
  assert.equal(items[0]?.price, 2, '切换不换算单价：2.00 仍是 2.00');

  const after = computeTotals(mode, items);
  assert.equal(after.total, 200, '含税口径：价税合计 200.00（卡面验收 ③）');
  assert.equal(after.amount, 176.99);
  assert.equal(after.tax, 23.01);

  // 多行也要各自按**本行**税率重算，汇总仍满足 amount + tax === total
  const multi = [line(), line({ key: 'row-2', qty: 3, price: 10, tax_rate: 6 })];
  const multiTotals = computeTotals('INCL', multi);
  assert.equal(multiTotals.total, 230, '200 + 30');
  assert.equal(multiTotals.amount, 205.29, '176.99 + 28.30');
  assert.equal(multiTotals.tax, 24.71, '23.01 + 1.70');
  assert.equal(
    Number((multiTotals.amount + multiTotals.tax).toFixed(2)),
    multiTotals.total,
    '合计也满足 amount + tax === total',
  );
});

// ==================== ④ 状态按钮显隐 ====================

test('④ 收货单状态按钮显隐：DRAFT 显示编辑/过账，POSTED 只显示查看（作废另算）', () => {
  const draft = availableStockInActions('DRAFT');
  assert.equal(draft.edit, true, 'DRAFT 可编辑');
  assert.equal(draft.post, true, 'DRAFT 可过账');
  assert.equal(draft.view, true, 'DRAFT 可查看');
  assert.equal(draft.remove, true, 'DRAFT 可删除');

  const posted = availableStockInActions('POSTED');
  assert.equal(posted.edit, false, 'POSTED 不可编辑（只读）');
  assert.equal(posted.post, false, 'POSTED 不可再过账（重复过账 409 + 10003）');
  assert.equal(posted.remove, false, 'POSTED 不可删除');
  assert.equal(posted.view, true, 'POSTED 仍可查看');

  const cancelled = availableStockInActions('CANCELLED');
  assert.equal(cancelled.edit, false);
  assert.equal(cancelled.post, false);
  assert.equal(cancelled.view, true);

  // 采购单侧：只有 DRAFT 能编辑/提交/删除，终态只留查看（与 LEGAL_EDGES 一致）
  const poDraft = availablePurchaseActions('DRAFT');
  assert.deepEqual(
    [poDraft.edit, poDraft.submit, poDraft.remove],
    [true, true, true],
    'DRAFT：编辑 / 提交 / 删除 都亮',
  );
  const poSubmitted = availablePurchaseActions('SUBMITTED');
  assert.equal(poSubmitted.edit, false, 'SUBMITTED 不能编辑');
  assert.equal(poSubmitted.close, true, 'SUBMITTED 可关闭');
  assert.equal(poSubmitted.receive, true, 'SUBMITTED 可去收货');
  const poClosed = availablePurchaseActions('CLOSED');
  assert.deepEqual(
    [poClosed.edit, poClosed.submit, poClosed.close, poClosed.cancel, poClosed.receive],
    [false, false, false, false, false],
    'CLOSED 是终态：只剩查看',
  );
  assert.equal(poClosed.view, true);
});

// ==================== ⑤ 过账确认弹窗（PRD §3.3.2 硬要求） ====================

test('⑤ 过账弹窗：默认勾「发票已随货到达」，「发票未到」置灰且带 tooltip', () => {
  assert.equal(defaultInvoiceChoice(), 'RECEIVED', '默认勾选「发票已随货到达」');

  const received = INVOICE_OPTIONS.find((item) => item.value === 'RECEIVED');
  const estimate = INVOICE_OPTIONS.find((item) => item.value === 'ESTIMATE');
  assert.ok(received, '必须有「发票已随货到达」选项');
  assert.ok(estimate, '必须有「发票未到」选项');

  assert.equal(received.disabled, false, '「发票已随货到达」可选');
  assert.equal(estimate.disabled, true, '★「发票未到」必须置灰');
  assert.equal(estimate.tip, ESTIMATE_DISABLED_TIP, '置灰项要带 tooltip 说明开放批次');
  assert.match(ESTIMATE_DISABLED_TIP, /第 2 批/, 'tooltip 写明「暂估入库将在第 2 批开放」');

  // 选项 → 后端 invoice_received 布尔
  assert.equal(invoiceReceivedOf('RECEIVED'), true);
  assert.equal(invoiceReceivedOf('ESTIMATE'), false);

  // 过账请求体：不带 reason / serials（系统在过账事务里自动取号）
  assert.deepEqual(buildPostPayload(), { status: 'POSTED' });
  // 作废必须带原因（POSTED → CANCELLED 缺 reason 后端 400）
  assert.deepEqual(buildCancelPayload('  页面作废 '), { status: 'CANCELLED', reason: '页面作废' });
});

// ==================== ⑥ 请求体：金额一律不传，单价按口径塞字段 ====================

test('⑥ 请求体：不含税口径带 price_excl，含税口径带 price_incl，且都不传金额字段', () => {
  const exclPayload = buildPurchasePayload({ ...emptyPurchaseForm(), supplier_id: 'sup-1', warehouse_id: 'wh-1', items: [line()] });
  assert.equal(exclPayload.price_mode, 'EXCL');
  assert.equal(exclPayload.items[0]?.price_excl, 2);
  assert.equal(exclPayload.items[0]?.price_incl, undefined, '不含税口径不发 price_incl');
  assert.equal('amount' in (exclPayload.items[0] ?? {}), false, '★ 不传金额：服务端算');
  assert.equal('total_with_tax' in exclPayload, false, '★ 单头金额也不传');

  const exclForm = { ...emptyPurchaseForm(), supplier_id: 'sup-1', warehouse_id: 'wh-1', items: [line()] };
  exclForm.price_mode = 'INCL';
  const inclPayload = buildPurchasePayload(exclForm);
  assert.equal(inclPayload.price_mode, 'INCL');
  assert.equal(inclPayload.items[0]?.price_incl, 2, '含税口径发 price_incl');
  assert.equal(inclPayload.items[0]?.price_excl, undefined);

  // 收货单：只有 price_excl（成本基础），invoice_received 默认 true
  const stockInPayload = buildStockInPayload({
    ...emptyStockInForm(),
    supplier_id: 'sup-1',
    warehouse_id: 'wh-1',
    items: [inLine()],
  });
  assert.equal(stockInPayload.invoice_received, true, '建单默认「发票已随货到达」');
  assert.equal(stockInPayload.items[0]?.price_excl, 2);
  assert.equal(stockInPayload.items[0]?.qty, 100);
  assert.equal('amount' in (stockInPayload.items[0] ?? {}), false, '收货明细同样不传金额');

  // 收货合计：100×2, 13% → 200 / 26 / 226
  const totals = computeStockInTotals([inLine()]);
  assert.equal(totals.amount, 200);
  assert.equal(totals.tax, 26);
  assert.equal(totals.total, 226);
  assert.equal(formatStockInMoney(totals.total), '226.00');
});

// ==================== ⑦ 表单校验（与后端同规则，少跑一次 400） ====================

test('⑦ 表单校验：0 数量 / 负单价 / 越界税率 / 缺供应商都能拦住', () => {
  // A6：采购单默认「合同采购」（供应商由合同带），本组用例测的是**现货采购**手填供应商那套校验
  const ok = {
    ...emptyPurchaseForm(),
    purchase_type: 'SPOT' as const,
    supplier_id: 'sup-1',
    warehouse_id: 'wh-1',
    items: [line()],
  };
  assert.deepEqual(validatePurchaseForm(ok), {}, '合法表单无错误');

  const zeroQty = validatePurchaseForm({ ...ok, items: [line({ qty: 0 })] });
  assert.match(zeroQty['items.0.qty'] ?? '', /大于 0/, 'ck_biz_po_item_qty 要求 qty > 0');

  const negative = validatePurchaseForm({ ...ok, items: [line({ price: -1 })] });
  assert.match(negative['items.0.price'] ?? '', /不能为负/);

  const badRate = validatePurchaseForm({ ...ok, items: [line({ tax_rate: 101 })] });
  assert.match(badRate['items.0.tax_rate'] ?? '', /0–100/);

  const noSupplier = validatePurchaseForm({ ...ok, supplier_id: '' });
  assert.equal(noSupplier.supplier_id, '请选择供应商');

  const stockInErrors = validateStockInForm({
    ...emptyStockInForm(),
    purchase_order_id: 'po-1',
    supplier_id: 'sup-1',
    warehouse_id: 'wh-1',
    items: [inLine({ qty: 0 })],
  });
  assert.match(stockInErrors['items.0.qty'] ?? '', /大于 0/, '收货同样拦 0 数量（0 数量后端 400）');
});

// ==================== ⑧ 数值归一 / 状态流转体 ====================

test('⑧ NUMERIC 字符串归一 + 取消带原因', () => {
  // 后端金额是字符串（"226.0000"），直接用 + 会变成字符串拼接
  assert.equal(toNumber('226.0000'), 226);
  assert.equal(toNumber('0'), 0);
  assert.equal(toNumber(null), 0);
  assert.equal(toNumber('abc'), 0);
  assert.equal(formatMoney('226.0000'), '226.00');
  assert.equal(formatMoney(12345.6), '12,345.60');

  assert.deepEqual(buildStatusPayload('SUBMITTED'), { status: 'SUBMITTED' }, '无事由不发 reason 字段');
  assert.deepEqual(buildStatusPayload('CANCELLED', ' 需求取消 '), {
    status: 'CANCELLED',
    reason: '需求取消',
  });
});

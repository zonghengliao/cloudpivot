/**
 * 批次成本页纯逻辑单测（M5-07 · 分批法前端）。
 *
 * 重点钉六件事（都是「算错就出账」的地方）：
 *
 * ① **期间必填 + 兜底**：非法 / 空期间必须回落本月（后端 `period_of` 直接 400）；
 * ② **两套金额形状不混**：归集表是 `mat/labor/oh`、在制是 `material/labor/overhead`，
 *    相加走整数分（`0.1 + 0.2` 用浮点会得 `0.30000000000000004`）；
 * ③ **入参拼装**：`ALL` / 空串 / `false` 整键缺席，`period_code` 永远发；
 * ④ **可转判定只有两条**（闸门空 + 有钱可转），且置灰原因必须给出一句人话；
 * ⑤ **来源三态与摘要**：过期将清 / 不可归集都要说出来（否则会被当成丢数）；
 * ⑥ **台账自洽**：逐期「期初 + 投入 − 转出 = 期末」、「下期期初 = 上期期末」、
 *    「最后一期期末 = 汇总余额」三条任一破就报出来。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import type {
  AmountsRes,
  BatchCollect,
  BatchFinishPreview,
  BatchSources,
  ElementSources,
  SheetAmounts,
  SheetRow,
  WipAmounts,
  WipLedger,
  WipRow,
} from '@/api/business/erp/cost-batch-kinds';
import {
  blockerLines,
  buildSheetQuery,
  buildWipQuery,
  canFinish,
  collectResultText,
  defaultPeriod,
  elementLabel,
  elementSourcesOf,
  elementSummaryText,
  emptySheetFilter,
  emptyWipFilter,
  finishDisabledReason,
  finishSummaryText,
  flattenLedgerRows,
  flattenSheetRow,
  flattenWipRow,
  ledgerMismatches,
  ledgerOpeningText,
  onlyWipFlag,
  pctText,
  periodOrFallback,
  periodSpan,
  sheetAmountsEmpty,
  sheetTotalCents,
  sourceStateLabel,
  sourceStateTheme,
  statusLabel,
  statusTheme,
  trimDecimal,
  warningTheme,
  wipAmountsNegative,
  wipTotalCents,
} from '@/views/business/erp/cost-batch/cost-batch-logic';

// ==================== 夹具 ====================

function sheetAmounts(mat = '0', labor = '0', oh = '0'): SheetAmounts {
  const total = (Number(mat) + Number(labor) + Number(oh)).toFixed(4);
  return { mat, labor, oh, total };
}

function wipAmounts(material = '0', labor = '0', overhead = '0'): WipAmounts {
  const total = (Number(material) + Number(labor) + Number(overhead)).toFixed(4);
  return { material, labor, overhead, total };
}

function amountsRes(material = '0', labor = '0', overhead = '0'): AmountsRes {
  const total = (Number(material) + Number(labor) + Number(overhead)).toFixed(4);
  return { material, labor, overhead, total };
}

function sheetRow(overrides: Partial<SheetRow> = {}): SheetRow {
  return {
    object_id: '11111111-1111-4111-8111-111111111111',
    object_no: 'CB-202611-000001',
    object_type: 'BATCH',
    source_doc_no: 'SO-2026-018',
    lot_no: 'LOT-A',
    product_id: '22222222-2222-4222-8222-222222222222',
    product_code: 'P-001',
    product_name: '支架',
    workshop_dept_id: '33333333-3333-4333-8333-333333333333',
    workshop_dept_name: '装配车间',
    plan_qty: '100.0000',
    completed_qty: '40.0000',
    wip_qty: '60.0000',
    completion_pct: '40.0000',
    status: 'OPEN',
    start_date: '2026-11-02',
    due_date: null,
    // 金额是**扁的**（后端 `#[serde(flatten)]`，见 cost-batch-kinds 的 SheetTotals 注释）
    mat: '1200.0000',
    labor: '320.5000',
    oh: '88.2500',
    total: '1608.7500',
    line_count: 6,
    last_occur_date: '2026-11-18',
    ...overrides,
  };
}

function wipRow(overrides: Partial<WipRow> = {}): WipRow {
  return {
    object_id: '11111111-1111-4111-8111-111111111111',
    object_no: 'CB-202611-000001',
    object_type: 'BATCH',
    source_doc_no: null,
    lot_no: null,
    product_id: '22222222-2222-4222-8222-222222222222',
    product_code: 'P-001',
    product_name: '支架',
    workshop_dept_id: '33333333-3333-4333-8333-333333333333',
    workshop_dept_name: '装配车间',
    plan_qty: '100.0000',
    completed_qty: '40.0000',
    wip_qty: '60.0000',
    completion_pct: '40.0000',
    status: 'OPEN',
    start_date: '2026-11-02',
    due_date: null,
    collected: wipAmounts('1200.0000', '320.5000', '88.2500'),
    transferred: wipAmounts('0', '0', '0'),
    balance: wipAmounts('1200.0000', '320.5000', '88.2500'),
    period_in: wipAmounts('1200.0000', '320.5000', '88.2500'),
    period_out: wipAmounts('0', '0', '0'),
    collect_lines: 6,
    transfer_times: 0,
    first_period: '2026-11',
    last_period: '2026-11',
    last_transfer_at: null,
    has_balance: true,
    warnings: [],
    ...overrides,
  };
}

function element(overrides: Partial<ElementSources> = {}): ElementSources {
  return {
    element: 'MATERIAL',
    element_label: '直接材料',
    rows: [],
    rows_total: 0,
    amount_total: '0',
    collected_rows: 0,
    collected_amount: '0',
    pending_rows: 0,
    pending_amount: '0',
    purged_rows: 0,
    purged_amount: '0',
    uncollected_rows: 0,
    uncollected_amount: '0',
    after_amount: '0',
    ...overrides,
  };
}

function preview(overrides: Partial<BatchFinishPreview> = {}): BatchFinishPreview {
  const batch = sheetRow();
  return {
    batch: { ...batch },
    period_code: '2026-11',
    period_batch: null,
    collected: amountsRes('1608.7500', '0', '0'),
    transferred: amountsRes('0', '0', '0'),
    pending: amountsRes('1608.7500', '0', '0'),
    finished_qty: '40.0000',
    unit_cost: '40.218750',
    cumulative_unit_cost: '40.218750',
    transferable: true,
    transfer_no: 1,
    settled_in_books: false,
    history: [],
    blockers: [],
    warnings: [],
    ...overrides,
  };
}

function ledger(periods: WipLedger['periods'], summaryBalance = wipAmounts('0', '0', '0')): WipLedger {
  return {
    period_code: '2026-11',
    batch: { ...sheetRow() },
    opening: wipAmounts(),
    summary: {
      collected: wipAmounts('1200.0000', '0', '0'),
      transferred: wipAmounts('300.0000', '0', '0'),
      balance: summaryBalance,
      collect_lines: 3,
      transfer_times: 1,
    },
    periods,
    truncated: false,
    after_period: null,
    settled: null,
    warnings: [],
    notes: [],
  };
}

// ==================== ① 期间 ====================

test('期间：北京时间跨月当天算下月（与后端 today_cst 同口径）', () => {
  // 2026-11-30T16:30Z = 北京时间 2026-12-01 00:30
  assert.equal(defaultPeriod(new Date('2026-11-30T16:30:00Z')), '2026-12');
  assert.equal(defaultPeriod(new Date('2026-11-30T15:30:00Z')), '2026-11');
});

test('期间兜底：空 / 非法形状一律回落本月，合法值原样返回', () => {
  const now = new Date('2026-11-19T02:00:00Z');
  assert.equal(periodOrFallback('', now), '2026-11');
  assert.equal(periodOrFallback(null, now), '2026-11');
  assert.equal(periodOrFallback('2026-13', now), '2026-11');
  assert.equal(periodOrFallback('2026-1', now), '2026-11');
  assert.equal(periodOrFallback('2026-03', now), '2026-03');
  assert.equal(periodOrFallback(' 2026-03 ', now), '2026-03');
});

// ==================== 数量 / 金额 ====================

test('数量裁剪：去小数尾零，空值给 0，不为负零', () => {
  assert.equal(trimDecimal('12.0000'), '12');
  assert.equal(trimDecimal('1.5000'), '1.5');
  assert.equal(trimDecimal('0.0000'), '0');
  assert.equal(trimDecimal(''), '0');
  assert.equal(trimDecimal(null), '0');
  assert.equal(trimDecimal('-2.2500'), '-2.25');
  assert.equal(trimDecimal('-0.0000'), '0');
});

test('完工率：百分数串不再乘 100，空值给破折号', () => {
  assert.equal(pctText('66.6700'), '66.67%');
  assert.equal(pctText('100.0000'), '100%');
  assert.equal(pctText(''), '—');
  assert.equal(pctText(null), '—');
});

test('归集口径合计走整数分：0.1 + 0.2 = 30 分（不是 0.30000000000000004）', () => {
  assert.equal(sheetTotalCents(sheetAmounts('0.1', '0.2', '0')), 30);
  assert.equal(wipTotalCents(wipAmounts('0.1', '0.2', '0')), 30);
  // 后端给的 `total` 字段不参与相加：三项和才是唯一口径（不等就是后端 bug，页面要看得出来）
  const tampered: SheetAmounts = { mat: '1', labor: '2', oh: '3', total: '999' };
  assert.equal(sheetTotalCents(tampered), 600);
});

test('有没有钱 / 是否为负：空判据与负判据互不干扰', () => {
  assert.equal(sheetAmountsEmpty(sheetAmounts()), true);
  assert.equal(sheetAmountsEmpty(sheetAmounts('0', '0', '0.01')), false);
  assert.equal(wipAmountsNegative(wipAmounts()), false);
  assert.equal(wipAmountsNegative(wipAmounts('0', '-0.01', '0')), true);
});

// ==================== 表格行拍平 ====================

test('归集表行拍平：金额是扁的、合计由三项相加（忽略后端 total 字段）、数量裁剪', () => {
  const row = flattenSheetRow(sheetRow({ mat: '1200.0000', labor: '320.5000', oh: '88.2500', total: '0' }));
  assert.equal(row.mat, '1200.0000');
  assert.equal(row.total_amount, '1,608.75');
  assert.equal(row.pct_text, '40%');
  assert.equal(row.plan_qty_text, '100');
  assert.equal(row.wip_qty_text, '60');
});

test('在制余额行拍平：余额三项 + 负数标记 + 期间跨度', () => {
  const row = flattenWipRow(
    wipRow({
      collected: wipAmounts('100.0000', '50.0000', '0'),
      transferred: wipAmounts('120.0000', '0', '0'),
      balance: wipAmounts('-20.0000', '50.0000', '0'),
      first_period: '2026-10',
      last_period: '2026-11',
    }),
  );
  assert.equal(row.collected_total, '150.00');
  assert.equal(row.transferred_total, '120.00');
  assert.equal(row.balance_total, '30.00');
  assert.equal(row.balance_mat, '-20.00');
  assert.equal(row.negative, true);
  assert.equal(row.period_span, '2026-10 ~ 2026-11');
});

test('期间跨度：单头 / 全空都要给得出来', () => {
  assert.equal(periodSpan('2026-11', '2026-11'), '2026-11');
  assert.equal(periodSpan('2026-11', null), '2026-11');
  assert.equal(periodSpan(null, null), '—');
});

// ==================== 入参拼装 ====================

test('归集表入参：period 永远发，ALL / 空串 / 空车间整键缺席', () => {
  const base = buildSheetQuery({ ...emptySheetFilter(), period_code: '2026-11' }, 2, 20);
  assert.equal(base.period_code, '2026-11');
  assert.equal(base.page, 2);
  assert.equal(base.page_size, 20);
  assert.equal(base.sort, 'total');
  assert.equal('keyword' in base, false);
  assert.equal('status' in base, false);
  assert.equal('workshop_dept_id' in base, false);

  const all = buildSheetQuery(
    { ...emptySheetFilter(), period_code: '2026-11', status: 'ALL' },
    1,
    20,
  );
  assert.equal('status' in all, false, 'ALL 是「不限」，不该发出去');

  const filled = buildSheetQuery(
    {
      period_code: '2026-11',
      keyword: '  SO-2026-018  ',
      status: 'OPEN',
      workshop_dept_id: '33333333-3333-4333-8333-333333333333',
      sort: 'order_no',
    },
    1,
    20,
  );
  assert.equal(filled.keyword, 'SO-2026-018');
  assert.equal(filled.status, 'OPEN');
  assert.equal(filled.workshop_dept_id, '33333333-3333-4333-8333-333333333333');
  assert.equal(filled.sort, 'order_no');
});

test('归集表入参：期间被清空时回落本月（防后端 400，不是猜月份）', () => {
  const query = buildSheetQuery({ ...emptySheetFilter(), period_code: '' }, 1, 20);
  assert.match(query.period_code, /^\d{4}-(0[1-9]|1[0-2])$/);
  assert.equal(query.period_code, defaultPeriod());
  // 形状非法（`2026-13`）同样回落，绝不把非法值发给后端
  assert.match(buildWipQuery({ ...emptyWipFilter(), period_code: '2026-13' }, 1, 20).period_code, /^\d{4}-(0[1-9]|1[0-2])$/);
});

test('在制余额入参：only_wip 下拉只把「1」翻成 true，其余整键缺席', () => {
  assert.equal(onlyWipFlag('1'), true);
  assert.equal(onlyWipFlag(''), undefined);
  assert.equal(onlyWipFlag('0'), undefined, '0 与默认同义：不发');

  const off = buildWipQuery({ ...emptyWipFilter(), period_code: '2026-11' }, 1, 20);
  assert.equal('only_wip' in off, false);
  assert.equal(off.sort, 'balance');

  const on = buildWipQuery({ ...emptyWipFilter(), period_code: '2026-11', only_wip: '1' }, 1, 20);
  assert.equal(on.only_wip, true);
});

// ==================== 词表 / 主题 ====================

test('词表：状态 / 要素 / 三态 / 警告级别都有中文，未知值不吞', () => {
  assert.equal(statusLabel('OPEN'), '在产');
  assert.equal(statusLabel('CLOSED'), '已关批');
  assert.equal(statusLabel('WEIRD'), 'WEIRD');
  assert.equal(statusTheme('OPEN'), 'primary');
  assert.equal(statusTheme('CLOSED'), 'success');
  assert.equal(statusTheme('CANCELLED'), 'default');
  assert.equal(elementLabel('MATERIAL'), '直接材料');
  assert.equal(elementLabel('X'), 'X');
  assert.equal(sourceStateLabel('PENDING'), '待归集');
  assert.equal(sourceStateTheme('COLLECTED'), 'success');
  assert.equal(sourceStateTheme('PENDING'), 'warning');
  assert.equal(sourceStateTheme('UNCOLLECTABLE'), 'default');
  assert.equal(warningTheme('WARN'), 'warning');
  assert.equal(warningTheme('INFO'), 'primary');
});

test('按要素取来源：三路各归各位（材料不会被拿到制造费用那一路）', () => {
  const sources = {
    material: element({ element: 'MATERIAL', element_label: '直接材料', rows_total: 1 }),
    labor: element({ element: 'LABOR', element_label: '直接人工', rows_total: 2 }),
    overhead: element({ element: 'OVERHEAD', element_label: '制造费用', rows_total: 3 }),
  } as unknown as BatchSources;
  assert.equal(elementSourcesOf(sources, 'MATERIAL')?.rows_total, 1);
  assert.equal(elementSourcesOf(sources, 'LABOR')?.rows_total, 2);
  assert.equal(elementSourcesOf(sources, 'OVERHEAD')?.rows_total, 3);
  assert.equal(elementSourcesOf(null, 'MATERIAL'), null);
});

// ==================== 来源摘要 / 归集回执 ====================

test('要素摘要：已归集 + 本次将写 + 归集后，缺省项不出现', () => {
  const text = elementSummaryText(
    element({
      collected_rows: 3,
      collected_amount: '1200.0000',
      pending_rows: 2,
      pending_amount: '800.0000',
      after_amount: '2000.0000',
    }),
  );
  assert.equal(text, '已归集 3 行 · 1,200.00 ｜ 本次将写 2 行 · 800.00 ｜ 归集后 2,000.00');
  assert.equal(text.includes('不可归集'), false);
  assert.equal(text.includes('过期将清'), false);
});

test('要素摘要：过期将清与不可归集都要说出来（否则会被当成丢数）', () => {
  const text = elementSummaryText(
    element({
      collected_rows: 3,
      collected_amount: '1200.0000',
      pending_rows: 1,
      pending_amount: '500.0000',
      purged_rows: 1,
      purged_amount: '100.0000',
      uncollected_rows: 2,
      uncollected_amount: '0',
      after_amount: '1600.0000',
    }),
  );
  assert.equal(
    text,
    '已归集 3 行 · 1,200.00 ｜ 本次将写 1 行 · 500.00 ｜ 过期将清 1 行 · 100.00 ｜ 不可归集 2 行 ｜ 归集后 1,600.00',
  );
});

test('归集回执：后端 message 打头，三段写入量逐要素列出，跳过量含「已归集」', () => {
  const leg = (label: string, overrides: Record<string, unknown> = {}) => ({
    element: label,
    element_label: label,
    scanned: 0,
    inserted: 0,
    skipped_existing: 0,
    purged: 0,
    inserted_amount: '0',
    purged_amount: '0',
    collected_rows: 0,
    collected_amount: '0',
    uncollected_rows: 0,
    uncollected_amount: '0',
    ...overrides,
  });
  const result = {
    period_code: '2026-11',
    batch: { ...sheetRow() },
    period_batch: null,
    recalc_required: false,
    material: leg('直接材料', { inserted: 2, inserted_amount: '800.0000', skipped_existing: 1, purged: 1, purged_amount: '20.0000' }),
    labor: leg('直接人工', { inserted: 1, inserted_amount: '320.0000' }),
    overhead: leg('制造费用'),
    totals: sheetAmounts('800.0000', '320.0000', '0'),
    message: '已归集 CB-202611-000001（2026-11）',
  } as unknown as BatchCollect;

  const text = collectResultText(result);
  const lines = text.split('\n');
  assert.equal(lines[0], '已归集 CB-202611-000001（2026-11）');
  assert.equal(
    lines[1],
    '直接材料：写 2 行 · 800.00，清 1 行 · 20.00，跳过 1 行（已归集）；' +
      '直接人工：写 1 行 · 320.00；制造费用：写 0 行 · 0.00',
  );
});

// ==================== 结转判定与文案 ====================

test('可转判定：闸门空 且 有钱可转，缺一不可；无试算一律不可转', () => {
  assert.equal(canFinish(preview()), true);
  assert.equal(canFinish(null), false);
  assert.equal(
    canFinish(preview({ transferable: false })),
    false,
    '钱已转完：再点只是幂等空转',
  );
  assert.equal(
    canFinish(
      preview({
        blockers: [
          {
            code: 'WIP_NOT_ZERO',
            message: '期末在制数量是 60（不为 0）',
            fix: '等整批完工后再结转',
          },
        ],
      }),
    ),
    false,
    '有闸门时不许点，哪怕有钱可转',
  );
});

test('置灰原因：先说闸门第一条，再说「无可转金额」', () => {
  assert.equal(finishDisabledReason(null), '结转试算未加载');
  assert.equal(
    finishDisabledReason(preview({ transferable: false })),
    '本次可结转金额是 0（累计归集已全部转出，无需再转）',
  );
  assert.equal(
    finishDisabledReason(
      preview({
        blockers: [{ code: 'NO_FINISHED_QTY', message: '累计完工数量是 0：没有产出就没有「完工转出」可结', fix: '先录完工数量' }],
      }),
    ),
    '累计完工数量是 0：没有产出就没有「完工转出」可结',
  );
  assert.equal(finishDisabledReason(preview()), '');
});

test('结转摘要：第几次 / 本次金额 / 完工数量 / 本次与累计单位成本', () => {
  assert.equal(
    finishSummaryText(preview({ transfer_no: 2, pending: amountsRes('600.0000', '0', '0') })),
    '第 2 次结转 ｜ 本次 600.00 ｜ 完工数量 40 ｜ 本次单位成本 40.21875 ｜ 结转后累计单位成本 40.21875',
  );
});

test('闸门文案：每条都带「怎么修」，不让人只看到「不行」', () => {
  const lines = blockerLines(
    preview({
      blockers: [
        { code: 'A', message: '还有 1 张领料出库单已过账但没有归集到本批', fix: '先对本批做归集，再结转' },
        { code: 'B', message: '还有 2 条草稿工时台账没确认', fix: '先确认（或删除）这些工时行' },
      ],
    }),
  );
  assert.equal(lines.length, 2);
  assert.equal(lines[0], '还有 1 张领料出库单已过账但没有归集到本批（怎么修：先对本批做归集，再结转）');
  assert.equal(lines[1], '还有 2 条草稿工时台账没确认（怎么修：先确认（或删除）这些工时行）');
  assert.deepEqual(blockerLines(null), []);
});

// ==================== 台账 ====================

test('台账自洽：三条判据都满足时零问题', () => {
  const data = ledger(
    [
      {
        period_code: '2026-10',
        opening: wipAmounts(),
        inflow: wipAmounts('1000.0000', '0', '0'),
        outflow: wipAmounts('0', '0', '0'),
        closing: wipAmounts('1000.0000', '0', '0'),
        collect_lines: 2,
        transfer_times: 0,
      },
      {
        period_code: '2026-11',
        opening: wipAmounts('1000.0000', '0', '0'),
        inflow: wipAmounts('200.0000', '0', '0'),
        outflow: wipAmounts('300.0000', '0', '0'),
        closing: wipAmounts('900.0000', '0', '0'),
        collect_lines: 1,
        transfer_times: 1,
      },
    ],
    wipAmounts('900.0000', '0', '0'),
  );
  assert.deepEqual(ledgerMismatches(data), []);
});

test('台账自洽：期末、滚动链、汇总三处各破一条都能报出来', () => {
  const broken = ledger(
    [
      {
        period_code: '2026-10',
        opening: wipAmounts(),
        inflow: wipAmounts('1000.0000', '0', '0'),
        outflow: wipAmounts('0', '0', '0'),
        closing: wipAmounts('1000.0000', '0', '0'),
        collect_lines: 2,
        transfer_times: 0,
      },
      {
        period_code: '2026-11',
        opening: wipAmounts('999.0000', '0', '0'), // ② 滚动链断
        inflow: wipAmounts('200.0000', '0', '0'),
        outflow: wipAmounts('300.0000', '0', '0'),
        closing: wipAmounts('950.0000', '0', '0'), // ① 期初+投入−转出 = 899 ≠ 950
        collect_lines: 1,
        transfer_times: 1,
      },
    ],
    wipAmounts('1.0000', '0', '0'), // ③ 与最后一期期末不等
  );
  const issues = ledgerMismatches(broken);
  assert.equal(issues.length, 3);
  assert.equal(issues[0], '2026-11：期初 + 投入 − 转出 = 899.00，但期末是 950.00');
  assert.equal(issues[1], '2026-11：期初 999.00 ≠ 上期期末 1,000.00');
  assert.equal(issues[2], '最后一期期末 950.00 ≠ 汇总在制余额 1.00');
  assert.deepEqual(ledgerMismatches(null), []);
});

test('台账行拍平与期初口径文案（截断要说出来）', () => {
  const data = ledger([
    {
      period_code: '2026-11',
      opening: wipAmounts('100.0000', '0', '0'),
      inflow: wipAmounts('50.0000', '0', '0'),
      outflow: wipAmounts('30.0000', '0', '0'),
      closing: wipAmounts('120.0000', '0', '0'),
      collect_lines: 1,
      transfer_times: 1,
    },
  ]);
  const rows = flattenLedgerRows(data);
  assert.equal(rows.length, 1);
  assert.equal(rows[0]?.opening_total, '100.00');
  assert.equal(rows[0]?.inflow_total, '50.00');
  assert.equal(rows[0]?.outflow_total, '30.00');
  assert.equal(rows[0]?.closing_total, '120.00');
  assert.deepEqual(flattenLedgerRows(null), []);

  assert.equal(ledgerOpeningText(data), '期初 0.00');
  assert.equal(
    ledgerOpeningText({ ...data, truncated: true }),
    '期初 0.00（含更早期间的净额，明细已截断）',
  );
  assert.equal(ledgerOpeningText(null), '—');
});

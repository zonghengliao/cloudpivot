/**
 * 在制品页纯逻辑单测（M5-25 · 消费 M5-21 工序完工率 + M5-23 期末在制余额）。
 *
 * 重点钉八件事（都是「显示错就会让人做出错决定」的地方）：
 *
 * ① **入参拼装**：余额期间必填且必须 `yyyy-MM`（非法 ⇒ 返回 `null`，页面**不发请求**）；
 *    空白关键字整键缺席；空状态整键缺席（发 `status=` 会查出 0 行）；
 * ② **百分比不乘 100**：后端 `completion_pct` 是百分数本身（`"50"` = 50%），
 *    与 `cost-variance` 的 `rate`（比值）**相反**；
 * ③ **`null` 不许画成 0**：余额算不出来 / 三要素不可用都显示「—」（`0.00` 会被读成「没有余额」）；
 * ④ **结转八态判色**：`carried` 绿、`mismatch`/`blocked` 红、`unsettled` 等黄，未知值回落；
 * ⑤ **合计直接取后端 `totals`**（服务端分页下前端不累加），未入账单列一档；
 * ⑥ **三要素固定料 / 工 / 费顺序**，后端漏一行也补占位而**不**补 0；
 * ⑦ **整表覆盖提交**：看得见的每一行都进 payload，`remark` 空 ⇒ `null`，`weight` 空 ⇒ 整键缺席；
 * ⑧ **前置校验与后端值域一致**：seq 1–999 且唯一、完工率必填且 0–100 / 2 位小数、
 *    权重 > 0 / 4 位小数、行数 ≤ 200、备注 ≤ 255。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import type {
  WipBalanceRes,
  WipBalanceRow,
  WipBalanceTotals,
  WipOperationRes,
} from '@/api/business/erp/cost-wip-kinds';
import {
  BALANCE_STATE_DEFAULT,
  CARRY_STATES,
  MAX_ROWS,
} from '@/api/business/erp/cost-wip-kinds';
import {
  DIRTY_ROLLUP_HINT,
  ELEMENTS_UNAVAILABLE_HINT,
  PERIOD_REQUIRED_HINT,
  amountOrDash,
  appendRow,
  attentionCount,
  buildBalanceQuery,
  buildObjectQuery,
  canSave,
  carryStateDef,
  carryText,
  carryTheme,
  defaultObjectFilter,
  editorRowsFromRes,
  elementLines,
  emptyTextOf,
  exactText,
  issueTexts,
  ledgerFacts,
  lockedHint,
  moneyText,
  needsAttention,
  nextSeq,
  noteText,
  objectEmptyText,
  pctText,
  productText,
  qtyText,
  quantityFacts,
  removeRow,
  rollupText,
  rowHints,
  saveErrorText,
  savedText,
  scopeHintOf,
  summaryCards,
  toSavePayload,
  validateRows,
  wipTone,
  type BalanceFilter,
  type EditorRow,
} from '@/views/business/erp/cost-wip/cost-wip-logic';

// ==================== 夹具 ====================

function balanceTotals(patch: Partial<WipBalanceTotals> = {}): WipBalanceTotals {
  return {
    row_count: 3,
    wip_row_count: 2,
    carried_count: 1,
    pending_count: 1,
    mismatch_count: 0,
    unsettled_count: 0,
    reversed_count: 0,
    closed_wip_count: 0,
    cleared_count: 1,
    blocked_count: 0,
    beg_qty: '10.0000',
    wip_qty: '40.0000',
    beg_total: '500.0000',
    cur_total: '3200.0000',
    finished_total: '3000.0000',
    wip_total: '700.0000',
    wip_material: '400.0000',
    wip_labor: '200.0000',
    wip_overhead: '100.0000',
    unbooked_total: '0.0000',
    closed_wip_total: '0.0000',
    mismatch_abs_total: '0.0000',
    element_row_count: 2,
    note: '合计按筛选后全集算',
    ...patch,
  };
}

function balanceRow(patch: Partial<WipBalanceRow> = {}): WipBalanceRow {
  return {
    object_id: 'oid-1',
    object_no: 'CB-20261119-000001',
    object_type: 'BATCH',
    object_status: 'OPEN',
    source_doc_no: null,
    lot_no: 'L-1',
    product_id: 'pid-1',
    product_code: 'P001',
    product_name: '半成品A',
    workshop_dept_id: 'dept-1',
    workshop_dept_name: '一车间',
    plan_qty: '100.0000',
    cumulative_completed_qty: '60.0000',
    completed_qty: '20.0000',
    beg_qty: '10.0000',
    beg_period: '2026-10',
    wip_qty: '40.0000',
    completion_pct: '50.00',
    beg_total: '500.0000',
    cur_total: '3200.0000',
    finished_total: '3000.0000',
    wip_total: '700.0000',
    elements: [
      { element: 'MATERIAL', label: '材料', beg: '300.0000', cur: '1000.0000', finished: '900.0000', wip: '400.0000' },
      { element: 'LABOR', label: '人工', beg: '150.0000', cur: '1500.0000', finished: '1450.0000', wip: '200.0000' },
      { element: 'OVERHEAD', label: '制造费用', beg: '50.0000', cur: '700.0000', finished: '650.0000', wip: '100.0000' },
    ],
    elements_available: true,
    source: 'settled',
    source_label: '已入账',
    ledger: {
      status: 'SETTLED',
      wip_end_qty: '40.0000',
      wip_end_cost: '700.0000',
      agrees: true,
      note: '试算与账面一致',
    },
    carry: {
      state: 'carried',
      label: '已结转',
      needs_attention: false,
      to_period: '2026-12',
      to_beg_qty: '40.0000',
      to_beg_cost: '700.0000',
      diff_qty: '0.0000',
      diff_cost: '0.0000',
      hint: '本期账面期末已一分不差地变成下期期初',
    },
    issues: [],
    warnings: [],
    ...patch,
  };
}

function balanceRes(patch: Partial<WipBalanceRes> = {}): WipBalanceRes {
  return {
    period_code: '2026-11',
    batch: { id: 'bid-1', batch_no: 'CC-202611-01', status: 'SETTLED' },
    reversed_batch: null,
    cost_ready: true,
    state: 'wip',
    keyword: null,
    formula_roll: '期末在制余额 = 期初 + 本期投入 − 完工转出（差额倒挤）',
    formula_carry: '结转 = 本期账面期末 → 下期期初',
    totals: balanceTotals(),
    list: [balanceRow()],
    total: 1,
    page: 1,
    page_size: 20,
    state_options: [{ value: 'wip', label: '有余额' }],
    note: '**合计**按筛选后全集算',
    ...patch,
  };
}

function editorRow(patch: Partial<EditorRow> = {}): EditorRow {
  return {
    key: `k-${Math.random()}`,
    seq: '1',
    operation_name: '下料',
    weight: '1',
    completion_pct: '100',
    remark: '',
    ...patch,
  };
}

function operationRes(patch: Partial<WipOperationRes> = {}): WipOperationRes {
  return {
    object: {
      id: 'oid-1',
      object_no: 'CB-20261119-000001',
      object_type: 'BATCH',
      product_code: 'P001',
      product_name: '半成品A',
      lot_no: 'L-1',
      workshop_dept_name: '一车间',
      status: 'OPEN',
      plan_qty: '100.0000',
      completed_qty: '60.0000',
      wip_qty: '40.0000',
      completion_pct: '50.00',
      material_input_mode: 'ONE_TIME',
      editable: true,
      blocked_reason: null,
    },
    rows: [
      {
        id: 'rid-1',
        seq: 1,
        operation_name: '下料',
        weight: '1.0000',
        completion_pct: '100.00',
        share_pct: '25.0000',
        contribution_pct: '25.0000',
        remark: null,
        updated_at: '2026-11-19T10:00:00Z',
      },
      {
        id: 'rid-2',
        seq: 2,
        operation_name: '焊接',
        weight: '2.0000',
        completion_pct: '50.00',
        share_pct: '50.0000',
        contribution_pct: '25.0000',
        remark: '关键工序',
        updated_at: '2026-11-19T10:00:00Z',
      },
      {
        id: 'rid-3',
        seq: 3,
        operation_name: '装配',
        weight: '1.0000',
        completion_pct: '0.00',
        share_pct: '25.0000',
        contribution_pct: '0.0000',
        remark: null,
        updated_at: '2026-11-19T10:00:00Z',
      },
    ],
    rollup: {
      row_count: 3,
      weight_sum: '4.0000',
      weighted_sum: '200.0000',
      pct_unrounded: '50.000000',
      pct: '50.00',
      object_pct: '50.00',
      matched: true,
      diff: '0.0000',
    },
    changed: false,
    inserted: 0,
    updated: 0,
    deleted: 0,
    state: 'editable',
    formula: '综合完工率 = Σ(工序权重 × 工序完工率) ÷ Σ工序权重',
    weight_hint: '权重一般填标准工时',
    warnings: [],
    note: '成本对象 CB-20261119-000001 共 3 道工序',
    ...patch,
  };
}

// ==================== ① 入参拼装 ====================

test('余额入参：期间非法 ⇒ null（页面不发请求）', () => {
  const filter: BalanceFilter = { period_code: '', keyword: '', state: 'wip' };
  assert.equal(buildBalanceQuery(filter, 1), null);
  assert.equal(buildBalanceQuery({ ...filter, period_code: '2026-13' }, 1), null);
  assert.equal(buildBalanceQuery({ ...filter, period_code: '2026-11-01' }, 1), null);
});

test('余额入参：期间合法 + 关键字去空白 + 状态缺省 wip', () => {
  const built = buildBalanceQuery({ period_code: ' 2026-11 ', keyword: '  CB-1  ', state: '' }, 2, 50);
  assert.ok(built);
  assert.equal(built!.period_code, '2026-11');
  assert.equal(built!.keyword, 'CB-1');
  assert.equal(built!.state, BALANCE_STATE_DEFAULT);
  assert.equal(built!.page, 2);
  assert.equal(built!.page_size, 50);
});

test('余额入参：空白关键字整键缺席（发 keyword= 会筛成 0 行）', () => {
  const built = buildBalanceQuery({ period_code: '2026-11', keyword: '   ', state: 'all' }, 1);
  assert.ok(built);
  assert.equal('keyword' in built!, false);
  assert.equal(built!.state, 'all');
});

test('余额入参：页码兜底（0 / 负数 ⇒ 1）', () => {
  const built = buildBalanceQuery({ period_code: '2026-11', keyword: '', state: 'wip' }, 0, 0);
  assert.equal(built!.page, 1);
  assert.equal(built!.page_size, 20);
});

test('对象入参：状态空 ⇒ 整键缺席；缺省筛选是「在产」', () => {
  assert.equal(defaultObjectFilter().status, 'OPEN');
  const built = buildObjectQuery({ keyword: '', status: '' }, 1);
  assert.equal('status' in built, false);
  assert.equal('keyword' in built, false);
  assert.equal(buildObjectQuery({ keyword: ' CB ', status: 'OPEN' }, 1).keyword, 'CB');
});

// ==================== ② 百分比不乘 100 ====================

test('完工率是百分数本身：pctText 不乘 100', () => {
  assert.equal(pctText('50'), '50%');
  assert.equal(pctText('22.23'), '22.23%');
  assert.equal(pctText('0.00'), '0%');
  assert.equal(pctText(null), '—');
});

test('数量去尾零 + 千分位；金额千分位 2 位', () => {
  assert.equal(qtyText('40.0000'), '40');
  assert.equal(qtyText('1234.5000'), '1,234.5');
  assert.equal(qtyText(null), '—');
  assert.equal(moneyText('700.0000'), '700.00');
  assert.equal(moneyText('12345.6789'), '12,345.68');
});

test('exactText 保住四位（尾差 0.0001 不许被抹成 0.00）', () => {
  assert.equal(exactText('413.1584'), '413.1584');
  assert.equal(exactText('0.0001'), '0.0001');
});

// ==================== ③ null 不画 0 ====================

test('金额 / 数量的 null 一律「—」（0.00 会被读成没有余额）', () => {
  assert.equal(amountOrDash(null), '—');
  assert.equal(amountOrDash(undefined), '—');
  assert.equal(amountOrDash('0.0000'), '0.00');
});

test('三要素不可用时表里仍是三行占位，不补 0', () => {
  const lines = elementLines([
    { element: 'MATERIAL', label: '材料', beg: null, cur: null, finished: null, wip: null },
  ]);
  assert.equal(lines.length, 3, '后端只回一行也要补齐料 / 工 / 费三行');
  assert.deepEqual(
    lines.map((line) => line.key),
    ['MATERIAL', 'LABOR', 'OVERHEAD'],
  );
  assert.equal(lines[0].wip, '—');
  assert.equal(lines[1].wip, '—', '没回的要素不许补 0');
  assert.ok(ELEMENTS_UNAVAILABLE_HINT.includes('红冲'), '说明里要给出修法');
});

// ==================== ④ 结转八态 ====================

test('八态齐全，判色与 attention 与后端一致', () => {
  assert.equal(CARRY_STATES.length, 8);
  assert.equal(carryTheme('carried'), 'success');
  assert.equal(carryTheme('mismatch'), 'danger');
  assert.equal(carryTheme('blocked'), 'danger');
  assert.equal(carryTheme('unsettled'), 'warning');
  assert.equal(carryTheme('cleared'), 'default');
  assert.equal(carryStateDef('carried').attention, false);
  assert.equal(carryStateDef('unsettled').attention, true);
  assert.equal(carryStateDef('closed_wip').label, '关批挂账');
});

test('未知状态回落 blocked 形态（不编「未知」，不抛）', () => {
  assert.equal(carryStateDef('some_new_state').value, 'blocked');
  // 后端给了 label 就用后端的
  assert.equal(
    carryText({ state: 'brand_new', label: '新状态' } as never),
    '新状态',
  );
  assert.equal(carryText(null), '—');
});

test('needs_attention 只认后端字段（前端不自己推）', () => {
  assert.equal(needsAttention({ state: 'mismatch', needs_attention: false } as never), false);
  assert.equal(needsAttention({ state: 'carried', needs_attention: true } as never), true);
});

// ==================== ⑤ 合计直接用后端 totals ====================

test('汇总八张卡，直接取后端 totals（不累加当前页）', () => {
  const cards = summaryCards(balanceRes());
  assert.equal(cards.length, 8);
  assert.deepEqual(
    cards.map((card) => card.key),
    ['rows', 'beg', 'cur', 'finished', 'wip', 'elements', 'unbooked', 'attention'],
  );
  const byKey = new Map(cards.map((card) => [card.key, card]));
  assert.equal(byKey.get('wip')!.value, '700.00');
  assert.equal(byKey.get('beg')!.value, '500.00');
  assert.equal(byKey.get('cur')!.value, '3,200.00');
  assert.equal(byKey.get('finished')!.value, '3,000.00');
  assert.ok(byKey.get('rows')!.sub.includes('有余额 2'), '行数卡副标题要带计数');
  assert.equal(summaryCards(null).length, 0);
});

test('未入账余额 > 0 标红（最危险的一档单列）', () => {
  const res = balanceRes({ totals: balanceTotals({ unbooked_total: '1200.0000', unsettled_count: 2 }) });
  const card = summaryCards(res).find((item) => item.key === 'unbooked');
  assert.equal(card!.tone, 'danger');
  assert.equal(card!.value, '1,200.00');
  assert.ok(card!.sub.includes('2 行'));
});

test('需要处理的行数 = 五档之和（carried / pending / cleared 不算）', () => {
  assert.equal(attentionCount(balanceTotals()), 0);
  assert.equal(
    attentionCount(
      balanceTotals({ mismatch_count: 1, unsettled_count: 2, reversed_count: 3, closed_wip_count: 4, blocked_count: 5 }),
    ),
    15,
  );
  const card = summaryCards(
    balanceRes({ totals: balanceTotals({ mismatch_count: 1 }) }),
  ).find((item) => item.key === 'attention');
  assert.equal(card!.value, '1');
  assert.equal(card!.tone, 'danger');
});

// ==================== ⑥ 展开行 / 话术 ====================

test('账面与结转事实：已结算行给出账面期末与结转差额', () => {
  const facts = ledgerFacts(balanceRow());
  const labels = facts.map((fact) => fact.label);
  assert.ok(labels.includes('账面期末'));
  assert.ok(labels.includes('结转状态'));
  assert.ok(labels.includes('下期期间'));
  assert.equal(facts.find((fact) => fact.label === '试算与账面')!.tone, 'success');
});

test('没结算的行：账面期末说人话（余额是试算值）', () => {
  const facts = ledgerFacts(balanceRow({ ledger: null, source: 'live', source_label: '试算未入账' }));
  const book = facts.find((fact) => fact.label === '账面期末');
  assert.ok(book!.value.includes('试算值'));
  assert.equal(book!.tone, 'danger');
});

test('quantityFacts 首期说明（beg_period = null）', () => {
  const facts = quantityFacts(balanceRow({ beg_period: null }));
  const beg = facts.find((fact) => fact.label === '期初在制');
  assert.ok(beg!.value.includes('首期'));
});

test('算不出来的行：issues 逐条给「事实 + 怎么修」', () => {
  const row = balanceRow({
    wip_total: null,
    elements_available: false,
    elements: [],
    carry: {
      state: 'blocked',
      label: '算不出',
      needs_attention: true,
      to_period: null,
      to_beg_qty: null,
      to_beg_cost: null,
      diff_qty: null,
      diff_cost: null,
      hint: '缺完工率，算不出加工约当',
    },
    issues: [{ code: 'NO_PCT', message: '期末在制 40 但完工率 0', fix: '录入工序完工率后重算' }],
  });
  assert.equal(wipTone(row), 'danger');
  const texts = issueTexts(row);
  assert.equal(texts.length, 1);
  assert.ok(texts[0].includes('怎么修'));
  assert.ok(rowHints(row).length >= 2, '先给 carry.hint，再给 issues');
});

test('正常已结转行判绿，算不出判红', () => {
  assert.equal(wipTone(balanceRow()), 'success');
  assert.equal(wipTone(balanceRow({ wip_total: null })), 'danger');
  assert.equal(
    wipTone(
      balanceRow({
        carry: { ...balanceRow().carry, state: 'unsettled', needs_attention: true },
      }),
    ),
    'danger',
  );
});

test('产出品列：编码 + 名称；主数据删了退到 id 前 8 位', () => {
  assert.equal(productText(balanceRow()), 'P001 半成品A');
  assert.equal(productText(balanceRow({ product_code: null, product_name: null })), 'pid-1'.slice(0, 8));
});

test('口径说明去 markdown 记号但不动原文', () => {
  assert.equal(noteText('**合计**按 `筛选后` 全集算'), '合计按 筛选后 全集算');
  assert.equal(noteText(null), '');
});

test('引导条：已红冲优先于「没有批次」；没有批次要说清「试算视图」', () => {
  const reversed = scopeHintOf(
    balanceRes({ reversed_batch: { id: 'b', batch_no: 'CC-1', status: 'REVERSED' } }),
  );
  assert.ok(reversed.includes('已红冲'));
  const noBatch = scopeHintOf(balanceRes({ cost_ready: false, batch: null }));
  assert.ok(noBatch.includes('试算视图'));
  assert.ok(noBatch.includes('成本核算'));
  assert.equal(scopeHintOf(balanceRes()), '');
});

test('空态文案区分「没选期间」「筛选没命中」', () => {
  assert.equal(emptyTextOf(null, { period_code: '', keyword: '', state: 'wip' }), PERIOD_REQUIRED_HINT);
  const mismatch = emptyTextOf(balanceRes({ state: 'mismatch', total: 0 }), {
    period_code: '2026-11',
    keyword: '',
    state: 'mismatch',
  });
  assert.ok(mismatch.includes('结转不齐'));
  const none = emptyTextOf(balanceRes({ state: 'wip', total: 0 }), {
    period_code: '2026-11',
    keyword: '',
    state: 'wip',
  });
  assert.ok(none.includes('没有在制余额行'));
});

test('录入页签空态文案：关键字 / 只有在产两种说法', () => {
  assert.ok(objectEmptyText({ keyword: 'ZZ', status: 'OPEN' }).includes('ZZ'));
  assert.ok(objectEmptyText({ keyword: '', status: 'OPEN' }).includes('在产'));
});

// ==================== ⑦ 整表覆盖提交 ====================

test('后端行 → 编辑行：字符串原样搬，不改标度', () => {
  const rows = editorRowsFromRes(operationRes());
  assert.equal(rows.length, 3);
  assert.equal(rows[1].weight, '2.0000');
  assert.equal(rows[2].completion_pct, '0.00');
  assert.equal(rows[0].remark, '');
});

test('提交 payload：看得见的每一行都进（整表覆盖），空备注 ⇒ null，空权重 ⇒ 整键缺席', () => {
  const rows = editorRowsFromRes(operationRes());
  rows[0].remark = '   ';
  rows[1].weight = '';
  const payload = toSavePayload(rows);
  assert.equal(payload.rows.length, 3);
  assert.equal(payload.rows[0].remark, null);
  assert.equal('weight' in payload.rows[1], false, '空权重整键缺席（后端缺省 1）');
  assert.equal(payload.rows[1].remark, '关键工序');
  assert.equal(payload.rows[0].seq, 1);
});

test('空表也是合法 payload（= 清空全部工序）', () => {
  assert.deepEqual(toSavePayload([]), { rows: [] });
});

test('加行 / 删行：序号取最大 +1，删了就提交时不带它', () => {
  const rows: EditorRow[] = [editorRow({ seq: '1' }), editorRow({ seq: '5' })];
  assert.equal(nextSeq(rows), 6);
  const added = appendRow(rows);
  assert.equal(added.seq, '6');
  assert.equal(rows.length, 3);
  removeRow(rows, rows[0].key);
  assert.equal(rows.length, 2);
  assert.equal(toSavePayload(rows).rows.length, 2);
});

test('nextSeq 封顶 999（不越库 CHECK）', () => {
  assert.equal(nextSeq([editorRow({ seq: '999' })]), 999);
});

// ==================== ⑧ 前置校验（值域与后端 specs_of 一致） ====================

test('合法三行 → 通过', () => {
  const result = validateRows(editorRowsFromRes(operationRes()));
  assert.equal(result.ok, true);
  assert.deepEqual(result.messages, []);
});

test('序号越界 / 非整数 / 重复都要点名', () => {
  const outOfRange = validateRows([editorRow({ seq: '0' })]);
  assert.equal(outOfRange.ok, false);
  assert.ok(outOfRange.rows[0].seq?.includes('1–999'));

  const decimal = validateRows([editorRow({ seq: '1.5' })]);
  assert.equal(decimal.ok, false);

  const dup = validateRows([editorRow({ seq: '2' }), editorRow({ seq: '2' })]);
  assert.equal(dup.ok, false);
  assert.ok(dup.rows[1].seq?.includes('重复'));
});

test('工序名必填 / 超 64 字', () => {
  assert.equal(validateRows([editorRow({ operation_name: '   ' })]).rows[0].operation_name, '必填');
  const long = validateRows([editorRow({ operation_name: '工'.repeat(65) })]);
  assert.ok(long.rows[0].operation_name?.includes('64'));
});

test('权重必须 > 0 / 最多 4 位小数 / 不超上限', () => {
  assert.ok(validateRows([editorRow({ weight: '0' })]).rows[0].weight?.includes('> 0'));
  assert.ok(validateRows([editorRow({ weight: '-1' })]).rows[0].weight?.includes('> 0'));
  assert.ok(validateRows([editorRow({ weight: '1.23456' })]).rows[0].weight?.includes('4 位小数'));
  assert.ok(validateRows([editorRow({ weight: '100000000' })]).rows[0].weight?.includes('99999999'));
  assert.equal(validateRows([editorRow({ weight: '' })]).ok, true, '空权重合法（后端缺省 1）');
});

test('完工率必填（不当 0） / 0–100 / 最多 2 位小数', () => {
  const missing = validateRows([editorRow({ completion_pct: '' })]);
  assert.equal(missing.ok, false);
  assert.ok(missing.rows[0].completion_pct?.includes('必填'));
  assert.ok(validateRows([editorRow({ completion_pct: '100.01' })]).rows[0].completion_pct?.includes('0–100'));
  assert.ok(validateRows([editorRow({ completion_pct: '-1' })]).rows[0].completion_pct?.includes('0–100'));
  assert.ok(validateRows([editorRow({ completion_pct: '33.333' })]).rows[0].completion_pct?.includes('2 位小数'));
  assert.equal(validateRows([editorRow({ completion_pct: '100' })]).ok, true);
  assert.equal(validateRows([editorRow({ completion_pct: '0' })]).ok, true);
});

test('备注 ≤ 255 字', () => {
  assert.equal(validateRows([editorRow({ remark: '备'.repeat(255) })]).ok, true);
  assert.ok(validateRows([editorRow({ remark: '备'.repeat(256) })]).rows[0].remark?.includes('255'));
});

test('行数上限 200（再多不是工序，是 BOM 明细）', () => {
  const rows = Array.from({ length: MAX_ROWS + 1 }, (_item, index) => editorRow({ seq: String(index + 1) }));
  const result = validateRows(rows);
  assert.equal(result.ok, false);
  assert.ok(result.messages.some((message) => message.includes('200')));
});

// ==================== 录入闸 / 文案 ====================

test('录入闸：editable 才能存；锁住时把后端原因整段转达', () => {
  assert.equal(canSave(operationRes()), true);
  const locked = operationRes({
    state: 'locked',
    object: { ...operationRes().object, editable: false, blocked_reason: '该批参与的成本批次已结算（2026-10(SETTLED)）：请先红冲' },
  });
  assert.equal(canSave(locked), false);
  assert.ok(lockedHint(locked).includes('红冲'));
  assert.equal(lockedHint(operationRes()), '');
});

test('汇总文案：没有工序时说清「沿用哪个值」', () => {
  assert.ok(rollupText(operationRes({ rollup: null })).includes('还没有工序'));
  const text = rollupText(operationRes());
  assert.ok(text.includes('3 道工序'));
  assert.ok(text.includes('50%'));
});

test('保存成功的播报带三个计数；409 补「怎么修」其余原样', () => {
  const saved = savedText(operationRes({ changed: true, inserted: 1, updated: 2, deleted: 1 }));
  assert.ok(saved.includes('新增 1'));
  assert.ok(saved.includes('更新 2'));
  assert.ok(saved.includes('删除 1'));
  assert.equal(savedText(operationRes({ changed: false })), '');

  const conflict = saveErrorText({ status: 409, message: '该批已关批' });
  assert.ok(conflict.includes('该批已关批'));
  assert.ok(conflict.includes('红冲'), '409 要给出路');
  assert.equal(saveErrorText({ status: 400, message: '不合法' }), '不合法');
  assert.equal(saveErrorText(null), '保存失败，请重试');
});

test('脏表提示说清「汇总是上次保存的值」', () => {
  assert.ok(DIRTY_ROLLUP_HINT.includes('上次保存'));
});

/**
 * 成本还原页纯逻辑单测（M5-30 · 消费 M5-27 还原明细 + M5-28 还原后报表）。
 *
 * 重点钉八件事（都是「显示错就会让人做出错决定」的地方）：
 *
 * ① **入参拼装**：期间必填且必须 `yyyy-MM`（非法 ⇒ 返回 `null`，页面**不发请求**）；
 *    空白关键字整键缺席；层数夹到 1–12；`group_by` 拼错回落 `dept`（后端会 400，别发坏值）；
 * ② **百分比不乘 100**：后端 `*_share` 是百分数本身（`"62.5"` = 62.5%），
 *    `*_pp` 是百分点差（带符号显示 `+13.71pp`）；
 * ③ **`share_sum` 不硬判 100**：`100.01` 是正常值（M5-29 四层链夹具实测），
 *    容差 ±0.02（`100.03` 才判 off）；
 * ④ **守恒自检**：`check` 为 0 才算平（整数分比较，无浮点假报警）；逐层台账每层也要平；
 * ⑤ **`null` 不许画成 0**：占比 `null` ⇒ `—`；`item = null` ⇒ 「本期还没算过」；
 * ⑥ **三要素固定料 / 工 / 费顺序**，展开行固定五行（料 / 工 / 费 / 合计 / 不彻底）；
 * ⑦ **合计直接用后端 `totals`**（服务端分页下前端不累加），卡片值与后端逐字一致；
 * ⑧ **话术**：不彻底 / 没有批次 / 空态 / 口径去 markdown，每一处都说清「为什么 + 下一步」。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import type {
  RestoreFlow,
  RestoreGroupRow,
  RestoreReportRes,
  RestoreRollupRes,
  RestoreRow,
  RestoreSemi,
} from '@/api/business/erp/cost-restore-kinds';
import {
  MAX_DEPTH_DEFAULT,
  MAX_DEPTH_LIMIT,
  PAGE_SIZE,
  REPORT_PAGE_SIZE,
  RESTORE_STATES,
  SHARE_SUM_TOLERANCE,
} from '@/api/business/erp/cost-restore-kinds';
import {
  PARTIAL_HINT,
  PERIOD_REQUIRED_HINT,
  buildReportQuery,
  buildRollupQuery,
  checkText,
  checkTone,
  defaultRestoreFilter,
  depthValue,
  elementLines,
  emptyTextOf,
  flowLines,
  groupLines,
  groupSelectOptions,
  hasPeriod,
  hintsOf,
  moneyText,
  noteText,
  objectTypeLabel,
  pctText,
  productText,
  reportBasis,
  reportCards,
  reportEmptyText,
  rollupBasis,
  rowFacts,
  scopeHintOf,
  semiLines,
  shareSumText,
  shareSumTone,
  signedAmount,
  signedPctText,
  stateLabelOf,
  stateSelectOptions,
  stateTheme,
  summaryCards,
  unresolvedTone,
  type RestoreFilter,
} from '@/views/business/erp/cost-restore/cost-restore-logic';

// ==================== 夹具 ====================

/**
 * 一行还原（**守恒恒成立**的夹具）：
 *
 * ```text
 * 还原前  料 1000（含半成品 600）· 工 100 · 费 100 = 1200
 * 还原后  料  300（外购）        · 工 300 · 费 400 = 1000   <- 料减 700，工 +200，费 +300
 * 不彻底  200                                        = 1200  ✅ 守恒
 * ```
 */
function makeRow(overrides: Partial<RestoreRow> = {}): RestoreRow {
  return {
    object_id: 'obj-1',
    object_no: 'CB-M530-F-0001',
    object_type: 'BATCH',
    lot_no: 'LOT-M530-0001',
    product_id: 'prod-1',
    product_code: 'F',
    product_name: '成品 F',
    workshop_dept_id: 'dept-1',
    workshop_dept_name: '总装车间',
    item: {
      status: 'SETTLED',
      completed_qty: '40.0000',
      finished_cost: '1400.0000',
      unit_finished_cost: '35.000000',
    },
    finished_cost: '1400.0000',
    before: { material: '1000', labor: '100', overhead: '100', total: '1200' },
    semi_in_material: '600',
    purchased_in_material: '400',
    semi_share: '60',
    after: { material: '300', labor: '300', overhead: '400', total: '1000' },
    after_material_share: '30',
    after_labor_share: '30',
    after_overhead_share: '40',
    delta: { material: '-700', labor: '200', overhead: '300', total: '0', note: '还原不动总额' },
    restored_labor: '200',
    restored_overhead: '300',
    unresolved: '200',
    state: 'partial',
    state_label: '还原不彻底',
    depth: 3,
    node_count: 7,
    flows: [],
    semis: [],
    issues: [],
    warnings: [],
    ...overrides,
  };
}

function makeFlow(overrides: Partial<RestoreFlow> = {}): RestoreFlow {
  return {
    level: 1,
    node_count: 4,
    imputed: '1000',
    purchased: '400',
    labor: '200',
    overhead: '200',
    carried_down: '200',
    unresolved: '0',
    note: '第 1 层 = 本期材料投入的去向',
    ...overrides,
  };
}

function makeSemi(overrides: Partial<RestoreSemi> = {}): RestoreSemi {
  return {
    level: 2,
    material_id: 'mat-semi-1',
    material_code: 'S2',
    material_name: '半成品 S2',
    lot_no: 'LOT-S2-0001',
    imputed: '600',
    source: 'batch',
    source_label: '按批次',
    producer_nos: 'CB-M530-S2-0001',
    elements: { material: '360', labor: '96', overhead: '144', total: '600' },
    unresolved: '0',
    stopped: false,
    ...overrides,
  };
}

/** 报表一组（占比四项合计 100.01 = M5-29 实测的正常值）。 */
function makeGroup(overrides: Partial<RestoreGroupRow> = {}): RestoreGroupRow {
  return {
    key: 'dept-1',
    label: '总装车间',
    hint: null,
    object_count: 2,
    before: {
      material: '1000',
      labor: '100',
      overhead: '100',
      total: '1200',
      material_share: '83.33',
      labor_share: '8.33',
      overhead_share: '8.33',
    },
    restored: {
      material: '300',
      labor: '300',
      overhead: '400',
      total: '1000',
      unresolved: '200',
      denominator: '1200',
      material_share: '25',
      labor_share: '25',
      overhead_share: '33.33',
      unresolved_share: '16.68',
    },
    shift: {
      material_pp: '-58.33',
      labor_pp: '16.67',
      overhead_pp: '25',
      note: '占比变化 = 还原后 − 还原前',
    },
    restored_labor: '200',
    restored_overhead: '300',
    semi_in_material: '600',
    semi_share: '60',
    restored_rows: 1,
    partial_rows: 1,
    no_semi_rows: 0,
    dominant: 'overhead',
    dominant_label: '制造费用最高',
    share_sum: '100.01',
    check: '0',
    ...overrides,
  };
}

function makeRollup(overrides: Partial<RestoreRollupRes> = {}): RestoreRollupRes {
  return {
    period_code: '2026-11',
    batch: { id: 'batch-1', batch_no: 'CS-202611-0001', status: 'SETTLED' },
    cost_ready: true,
    state: 'partial',
    keyword: null,
    max_depth: 6,
    formula_restore: '还原 = 材料里的自制半成品按结构递归拆开',
    formula_unresolved: '还原不彻底 = 停在半成品形态的金额',
    totals: {
      row_count: 2,
      restored_row_count: 1,
      partial_row_count: 1,
      no_semi_row_count: 0,
      before_material: '2000',
      before_labor: '200',
      before_overhead: '200',
      before_total: '2400',
      after_material: '600',
      after_labor: '600',
      after_overhead: '800',
      after_total: '2000',
      unresolved: '400',
      semi_in_material: '1200',
      check: '0',
      max_depth: 3,
      node_count: 14,
      after_material_share: '30',
      after_labor_share: '30',
      after_overhead_share: '40',
      note: '合计是整张表的（不受分页影响）',
    },
    list: [makeRow()],
    total: 2,
    page: 1,
    page_size: PAGE_SIZE,
    state_options: [{ value: 'all', label: '全部' }],
    note: '**还原**只把钱换个形态',
    ...overrides,
  };
}

function makeReport(overrides: Partial<RestoreReportRes> = {}): RestoreReportRes {
  return {
    period_code: '2026-11',
    batch: { id: 'batch-1', batch_no: 'CS-202611-0001', status: 'SETTLED' },
    cost_ready: true,
    group_by: 'dept',
    group_by_label: '按车间',
    group_options: [{ value: 'dept', label: '按车间' }],
    state: 'all',
    state_label: '全部',
    keyword: null,
    max_depth: 6,
    basis: '占比分母 = 还原前合计',
    formula_restore: '还原 = …',
    formula_unresolved: '不彻底 = …',
    formula_share: '占比 = 金额 ÷ 还原前合计 × 100',
    formula_shift: '占比变化（pp）= 还原后 − 还原前',
    totals: makeGroup({ key: 'ALL', label: '合计', hint: null }),
    groups: [makeGroup()],
    total: 1,
    page: 1,
    page_size: REPORT_PAGE_SIZE,
    state_options: [{ value: 'all', label: '全部' }],
    note: '报表是纯汇总',
    ...overrides,
  };
}

function makeFilter(overrides: Partial<RestoreFilter> = {}): RestoreFilter {
  return { ...defaultRestoreFilter(new Date('2026-11-20T10:00:00+08:00')), ...overrides };
}

// ==================== ① 入参拼装 ====================

test('buildRollupQuery：期间必填，非法一律返回 null（页面不发请求）', () => {
  assert.equal(buildRollupQuery(makeFilter({ period_code: '' }), 1), null);
  assert.equal(buildRollupQuery(makeFilter({ period_code: '2026-1' }), 1), null);
  assert.equal(buildRollupQuery(makeFilter({ period_code: '2026-13' }), 1), null);
  assert.equal(buildRollupQuery(makeFilter({ period_code: '202611' }), 1), null);
  assert.equal(hasPeriod(makeFilter({ period_code: '2026-11' })), true);
  assert.equal(hasPeriod(makeFilter({ period_code: '' })), false);
});

test('buildRollupQuery：合法期间 + 缺省值 + 空白关键字整键缺席', () => {
  const query = buildRollupQuery(makeFilter({ keyword: '   ' }), 2, PAGE_SIZE);
  assert.ok(query);
  assert.equal(query.period_code, '2026-11');
  assert.equal(query.state, 'all');
  assert.equal(query.page, 2);
  assert.equal(query.page_size, PAGE_SIZE);
  assert.equal(query.max_depth, MAX_DEPTH_DEFAULT);
  assert.equal('keyword' in query, false, '空白关键字必须整键缺席（发 keyword= 会筛成 0 行）');
});

test('buildRollupQuery：关键字 trim 后照发；页码 / 页大小非法回落到 1 / PAGE_SIZE', () => {
  const query = buildRollupQuery(makeFilter({ keyword: '  CB-M530  ', state: 'partial' }), 0, 0);
  assert.ok(query);
  assert.equal(query.keyword, 'CB-M530');
  assert.equal(query.state, 'partial');
  assert.equal(query.page, 1);
  assert.equal(query.page_size, PAGE_SIZE);
});

test('depthValue：0 / 负数 / 非数字 ⇒ 默认 6；超上限夹到 12；字符串数字照收', () => {
  assert.equal(depthValue(0), MAX_DEPTH_DEFAULT);
  assert.equal(depthValue(-3), MAX_DEPTH_DEFAULT);
  assert.equal(depthValue('' as unknown as string), MAX_DEPTH_DEFAULT);
  assert.equal(depthValue('abc' as unknown as string), MAX_DEPTH_DEFAULT);
  assert.equal(depthValue(99), MAX_DEPTH_LIMIT);
  assert.equal(depthValue('4'), 4);
  assert.equal(depthValue(3.9), 3);
  assert.equal(depthValue(12), 12);
});

test('buildReportQuery：维度透传 / 未知维度回落 dept / 期间非法返回 null', () => {
  assert.equal(buildReportQuery(makeFilter({ period_code: '' }), 1), null);
  const byProduct = buildReportQuery(makeFilter({ group_by: 'product' }), 1);
  assert.ok(byProduct);
  assert.equal(byProduct.group_by, 'product');
  assert.equal(byProduct.page_size, REPORT_PAGE_SIZE);
  const broken = buildReportQuery(makeFilter({ group_by: 'workshop' }), 1);
  assert.ok(broken);
  assert.equal(broken.group_by, 'dept', '拼错的维度不许发出去（后端 400），回落缺省');
});

test('defaultRestoreFilter：本月 + 全部状态 + 6 层 + 按车间', () => {
  const filter = defaultRestoreFilter(new Date('2026-11-20T10:00:00+08:00'));
  assert.equal(filter.period_code, '2026-11');
  assert.equal(filter.keyword, '');
  assert.equal(filter.state, 'all');
  assert.equal(filter.max_depth, MAX_DEPTH_DEFAULT);
  assert.equal(filter.group_by, 'dept');
});

test('下拉项从词表派生（两处各写一份必然漂移）', () => {
  assert.deepEqual(
    stateSelectOptions().map((option) => option.value),
    ['all', 'restored', 'partial', 'no_semi'],
  );
  assert.deepEqual(
    groupSelectOptions().map((option) => option.value),
    ['dept', 'product', 'object', 'all'],
  );
  assert.equal(stateSelectOptions().length, RESTORE_STATES.length);
});

// ==================== ② 数值 → 文字 ====================

test('moneyText：千分位 + 2 位；null / 空串 ⇒ 0.00（金额列专用）', () => {
  assert.equal(moneyText('1234.5'), '1,234.50');
  assert.equal(moneyText('0'), '0.00');
  assert.equal(moneyText('-700'), '-700.00');
  assert.equal(moneyText(null), '0.00');
});

test('pctText：不乘 100，null ⇒ 破折号（分母为 0 不是 0%）', () => {
  assert.equal(pctText('62.5'), '62.5%');
  assert.equal(pctText('100'), '100%');
  assert.equal(pctText(null), '—');
  assert.equal(pctText(''), '—');
  assert.notEqual(pctText('62.5'), '6250%', '后端给的是百分数本身，不许再乘 100');
});

test('signedPctText：带符号百分点（材料通常负 / 工费通常正）', () => {
  assert.equal(signedPctText('-58.33'), '-58.33pp');
  assert.equal(signedPctText('16.67'), '+16.67pp');
  assert.equal(signedPctText('0'), '+0pp');
  assert.equal(signedPctText(null), '—');
});

test('signedAmount：差额补符号，0 就是 0（合计差额恒为 0）', () => {
  assert.equal(signedAmount('-700'), '-700');
  assert.equal(signedAmount('200'), '+200');
  assert.equal(signedAmount('0'), '0');
  assert.equal(signedAmount('0.0000'), '0');
});

// ==================== ③④ 自检判色 ====================

test('shareSumTone：100.01 是正常值；容差 ±0.02 用整数分判', () => {
  assert.equal(SHARE_SUM_TOLERANCE, 0.02);
  assert.equal(shareSumTone('100'), 'ok');
  assert.equal(shareSumTone('100.00'), 'ok');
  assert.equal(shareSumTone('100.01'), 'ok', 'M5-29 实测 100.01，不许判红');
  assert.equal(shareSumTone('99.98'), 'ok');
  assert.equal(shareSumTone('100.02'), 'ok');
  assert.equal(shareSumTone('99.97'), 'off');
  assert.equal(shareSumTone('100.03'), 'off');
  assert.equal(shareSumTone(null), 'unknown');
  assert.equal(shareSumTone(''), 'unknown');
});

test('shareSumText：把实际值与容差写出来，不只说「正常」', () => {
  assert.match(shareSumText('100.01'), /100\.01%/);
  assert.match(shareSumText('100.01'), /舍入差/);
  assert.match(shareSumText('105'), /偏离 100%/);
  assert.match(shareSumText(null), /分母为 0/);
});

test('checkTone / checkText：守恒差额为 0 才算平（整数分，无浮点假报警）', () => {
  assert.equal(checkTone('0'), 'ok');
  assert.equal(checkTone('0.0000'), 'ok');
  assert.equal(checkTone('0.0001'), 'off');
  assert.equal(checkTone('-0.01'), 'off');
  assert.equal(checkTone(null), 'unknown');
  assert.match(checkText('0'), /守恒自检 0/);
  assert.match(checkText('0.0001'), /别拿这张表结账/);
});

// ==================== ⑤ 状态与文本 ====================

test('stateTheme / stateLabelOf：已还原绿、不彻底红、无需还原灰；后端 label 优先', () => {
  assert.equal(stateTheme('restored'), 'success');
  assert.equal(stateTheme('partial'), 'danger');
  assert.equal(stateTheme('no_semi'), 'default');
  assert.equal(stateLabelOf({ state: 'partial', state_label: '还原不彻底' }), '还原不彻底');
  assert.equal(stateLabelOf({ state: 'restored', state_label: '' }), '已还原');
});

test('productText / objectTypeLabel：主数据缺名不留白', () => {
  assert.equal(productText({ product_code: 'F', product_name: '成品 F', product_id: 'p' }), 'F 成品 F');
  assert.equal(productText({ product_code: 'F', product_name: '', product_id: 'p' }), 'F');
  assert.equal(productText({ product_code: '', product_name: '', product_id: '0123456789ab' }), '01234567');
  assert.equal(objectTypeLabel('BATCH'), '批次');
  assert.equal(objectTypeLabel('ORDER'), '订单');
  assert.equal(objectTypeLabel('PRODUCT'), '产品');
  assert.equal(objectTypeLabel(''), '—');
});

test('unresolvedTone：只有 partial 标红', () => {
  assert.equal(unresolvedTone({ state: 'partial' }), 'danger');
  assert.equal(unresolvedTone({ state: 'restored' }), 'default');
  assert.equal(unresolvedTone({ state: 'no_semi' }), 'default');
});

// ==================== ⑥⑦ 卡片与展开行 ====================

test('summaryCards：8 张、值逐字取自后端 totals（前端不累加）', () => {
  const cards = summaryCards(makeRollup());
  assert.equal(cards.length, 8);
  assert.deepEqual(
    cards.map((card) => card.key),
    ['rows', 'before', 'after_material', 'after_labor', 'after_overhead', 'unresolved', 'semi', 'check'],
  );
  const byKey = new Map(cards.map((card) => [card.key, card]));
  assert.equal(byKey.get('rows')?.value, '2');
  assert.match(byKey.get('rows')?.sub ?? '', /已还原 1 · 不彻底 1 · 无需还原 0/);
  assert.equal(byKey.get('before')?.value, '2,400.00');
  assert.equal(byKey.get('after_material')?.value, '600.00');
  assert.equal(byKey.get('unresolved')?.value, '400.00');
  assert.equal(byKey.get('unresolved')?.tone, 'danger');
  assert.equal(byKey.get('check')?.value, '0');
  assert.equal(byKey.get('check')?.tone, 'success');
  assert.equal(summaryCards(null).length, 0);
});

test('summaryCards：不彻底 0 行时该卡转绿并说明「没有钱停在半成品形态」', () => {
  const res = makeRollup();
  res.totals.partial_row_count = 0;
  res.totals.unresolved = '0';
  const card = summaryCards(res).find((item) => item.key === 'unresolved');
  assert.equal(card?.tone, 'success');
  assert.match(card?.sub ?? '', /全部拆到外购料/);
});

test('elementLines：固定五行（料 / 工 / 费 / 合计 / 不彻底），差额来自后端 delta', () => {
  const lines = elementLines(makeRow());
  assert.deepEqual(
    lines.map((line) => line.key),
    ['material', 'labor', 'overhead', 'total', 'unresolved'],
  );
  const byKey = new Map(lines.map((line) => [line.key, line]));
  assert.equal(byKey.get('material')?.before, '1,000.00');
  assert.equal(byKey.get('material')?.after, '300.00');
  assert.equal(byKey.get('material')?.delta, '-700');
  assert.equal(byKey.get('labor')?.delta, '+200');
  assert.equal(byKey.get('total')?.before, '1,200.00');
  assert.equal(byKey.get('total')?.after, '1,000.00');
  assert.equal(byKey.get('total')?.delta, '0');
  assert.equal(byKey.get('unresolved')?.before, '—');
  assert.equal(byKey.get('unresolved')?.after, '200.00');
});

test('flowLines：每层守恒自检为 0；破坏一项就标红', () => {
  const balanced = flowLines([makeFlow()]);
  assert.equal(balanced.length, 1);
  assert.equal(balanced[0].balanced, true);
  assert.equal(balanced[0].balance, '0');
  assert.equal(balanced[0].levelText, '第 1 层');
  assert.equal(balanced[0].imputed, '1,000');

  const broken = flowLines([makeFlow({ unresolved: '0.0001' })]);
  assert.equal(broken[0].balanced, false);
  assert.notEqual(broken[0].balance, '0');

  assert.deepEqual(flowLines(null), []);
  assert.deepEqual(flowLines([]), []);
});

test('flowLines：多层各算各的（各层不相加，层号与节点数照搬）', () => {
  const lines = flowLines([
    makeFlow({ level: 1, node_count: 4, imputed: '1000', carried_down: '200' }),
    makeFlow({ level: 2, node_count: 2, imputed: '200', purchased: '0', labor: '40', overhead: '40', carried_down: '120' }),
  ]);
  assert.deepEqual(
    lines.map((line) => line.levelText),
    ['第 1 层', '第 2 层'],
  );
  assert.equal(lines[1].imputed, '200', '第 2 层流入 = 上一层往下拨的钱，不是 1000');
  assert.equal(lines.every((line) => line.balanced), true);
});

test('semiLines：物料编码 + 名称、批号缺失画破折号、停层话术两态', () => {
  const stopped = semiLines([makeSemi({ stopped: true, lot_no: null, source_label: null, source: 'period_avg' })]);
  assert.equal(stopped[0].material, 'S2 半成品 S2');
  assert.equal(stopped[0].lotNo, '—');
  assert.equal(stopped[0].source, 'period_avg');
  assert.match(stopped[0].stoppedText, /停在这一层/);
  assert.equal(stopped[0].imputed, '600');
  assert.equal(stopped[0].materialCost, '360');

  const going = semiLines([makeSemi()]);
  assert.match(going[0].stoppedText, /已继续往下拆/);
  assert.equal(going[0].producer, 'CB-M530-S2-0001');
});

test('rowFacts：item 为 null 说「本期还没算过」，不画 0', () => {
  const noItem = rowFacts(makeRow({ item: null, finished_cost: null }));
  const itemFact = noItem.find((fact) => fact.label === '本期计算单');
  assert.ok(itemFact);
  assert.match(itemFact.value, /本期还没算过/);
  assert.match(itemFact.value, /照做没问题/);

  const withItem = rowFacts(makeRow());
  assert.match(withItem.find((fact) => fact.label === '本期计算单')?.value ?? '', /SETTLED/);
  assert.match(withItem.find((fact) => fact.label === '材料里的半成品')?.value ?? '', /60%/);
  assert.equal(withItem.find((fact) => fact.label === '生产车间')?.value, '总装车间');
});

test('hintsOf：issues 带「怎么修」；partial 且无原因给兜底；干净行返回空', () => {
  const withIssue = hintsOf(
    makeRow({ issues: [{ code: 'NO_STRUCTURE', message: '产出批次没算成本', fix: '到成本核算页算该批' }] }),
  );
  assert.equal(withIssue.length, 1);
  assert.match(withIssue[0], /产出批次没算成本（怎么修：到成本核算页算该批）/);

  const bare = hintsOf(makeRow({ issues: [], warnings: [] }));
  assert.equal(bare.length, 1);
  assert.match(bare[0], /批号对不上/);

  const clean = hintsOf(makeRow({ state: 'restored', unresolved: '0', issues: [], warnings: [] }));
  assert.deepEqual(clean, []);

  const warned = hintsOf(makeRow({ state: 'restored', unresolved: '0', warnings: ['层数到上限'] }));
  assert.deepEqual(warned, ['层数到上限']);
});

// ==================== 报表页签 ====================

test('reportCards：8 张，占比取还原后、变化取 pp、两个自检各自判色', () => {
  const cards = reportCards(makeReport());
  assert.equal(cards.length, 8);
  const byKey = new Map(cards.map((card) => [card.key, card]));
  assert.equal(byKey.get('material')?.value, '25%');
  assert.match(byKey.get('material')?.sub ?? '', /还原前 83\.33%/);
  assert.match(byKey.get('material')?.sub ?? '', /-58\.33pp/);
  assert.equal(byKey.get('labor')?.value, '25%');
  assert.equal(byKey.get('overhead')?.value, '33.33%');
  assert.equal(byKey.get('unresolved')?.value, '16.68%');
  assert.equal(byKey.get('unresolved')?.tone, 'danger', '有 partial_rows 就该标红');
  assert.equal(byKey.get('share_sum')?.tone, 'success', '100.01 在容差内');
  assert.equal(byKey.get('check')?.value, '0');
  assert.equal(reportCards(null).length, 0);
});

test('reportCards：占比例外（105）判红', () => {
  const res = makeReport();
  res.totals = makeGroup({ share_sum: '105' });
  const card = reportCards(res).find((item) => item.key === 'share_sum');
  assert.equal(card?.tone, 'danger');
});

test('groupLines：一页分组逐行映射（含 tooltip 用的原文）', () => {
  const lines = groupLines(makeReport());
  assert.equal(lines.length, 1);
  const line = lines[0];
  assert.equal(line.label, '总装车间');
  assert.equal(line.objectCount, 2);
  assert.equal(line.beforeTotal, '1,200.00');
  assert.match(line.beforeElements, /料 1,000\.00/);
  assert.equal(line.afterTotal, '1,000.00');
  assert.equal(line.materialShare, '25%');
  assert.equal(line.materialPp, '-58.33pp');
  assert.equal(line.overheadPp, '+25pp');
  assert.equal(line.unresolvedShare, '16.68%');
  assert.equal(line.shareSum, '100.01%');
  assert.equal(line.shareSumTone, 'ok');
  assert.equal(line.shareRaw, '100.01', 'tooltip 要显示后端原文');
  assert.equal(line.check, '0');
  assert.equal(line.checkRaw, '0');
  assert.equal(line.dominant, '制造费用最高');
  assert.equal(line.semiShare, '60%');
  assert.match(line.rowCounts, /已还原 1 · 不彻底 1 · 无需还原 0/);
  assert.deepEqual(groupLines(null), []);
});

test('groupLines：按对象维度带副标（产出品 · 批号）', () => {
  const res = makeReport();
  res.groups = [makeGroup({ key: 'obj-1', label: 'CB-M530-F-0001', hint: 'F 成品 F · 批号 LOT-M530-0001' })];
  const line = groupLines(res)[0];
  assert.equal(line.hint, 'F 成品 F · 批号 LOT-M530-0001');
  assert.equal(line.label, 'CB-M530-F-0001');
});

// ==================== ⑧ 话术 / 空态 / 口径 ====================

test('scopeHintOf：没有成本核算批次时说明「还原照做」；有批次则为空', () => {
  assert.equal(scopeHintOf(makeRollup()), '');
  const noBatch = makeRollup({ batch: null, cost_ready: false });
  const hint = scopeHintOf(noBatch);
  assert.match(hint, /还没有成本核算批次/);
  assert.match(hint, /还原照做/);
  assert.equal(scopeHintOf(null), '');
});

test('emptyTextOf：没选期间 / 没取到 / 状态筛掉三种空态说三种话', () => {
  assert.equal(emptyTextOf(null, makeFilter({ period_code: '' })), PERIOD_REQUIRED_HINT);
  assert.match(emptyTextOf(null, makeFilter()), /还没有取到数据/);
  assert.match(emptyTextOf(makeRollup(), makeFilter({ state: 'restored' })), /没有「已还原」的行/);
  assert.match(emptyTextOf(makeRollup(), makeFilter()), /没有可还原的成本对象/);
});

test('reportEmptyText：没期间 / 没取到 / 没数据三种空态', () => {
  assert.equal(reportEmptyText(null, makeFilter({ period_code: '' })), PERIOD_REQUIRED_HINT);
  assert.match(reportEmptyText(null, makeFilter()), /还没有取到数据/);
  assert.match(reportEmptyText(makeReport(), makeFilter()), /没有可汇总的还原结果（维度：按车间）/);
});

test('noteText：去掉 markdown 记号但不动口径原文', () => {
  assert.equal(noteText('**还原**只把钱换个 `形态`\n第二行'), '还原只把钱换个 形态 第二行');
  assert.equal(noteText(null), '');
});

test('rollupBasis / reportBasis：口径三段合并、去记号、去空段', () => {
  const rollup = rollupBasis(makeRollup());
  assert.match(rollup, /还原 = 材料里的自制半成品按结构递归拆开/);
  assert.match(rollup, /还原不彻底 = 停在半成品形态的金额/);
  assert.doesNotMatch(rollup, /\*\*/);
  assert.equal(rollupBasis(null), '');

  const basis = reportBasis(makeReport());
  assert.match(basis, /占比分母 = 还原前合计/);
  assert.match(basis, /占比变化（pp）= 还原后 − 还原前/);
  assert.equal(reportBasis(null), '');
});

test('PARTIAL_HINT：讲清「既不算外购料也不丢」与守恒等式', () => {
  assert.match(PARTIAL_HINT, /还原后合计 \+ 还原不彻底 = 还原前合计/);
  assert.match(PARTIAL_HINT, /既不算进外购材料/);
});

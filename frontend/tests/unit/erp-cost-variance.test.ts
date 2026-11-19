/**
 * 差异分析页纯逻辑单测（M5-19 · 消费 M5-11 的逐物料差异报表）。
 *
 * 重点钉六件事（都是「显示错就会让人做出错决定」的地方）：
 *
 * ① **入参拼装**：期间必填且必须是 `yyyy-MM`（非法 ⇒ 返回 `null`，页面**不发请求**）；
 *    空白关键字整键缺席（发 `keyword=` 会筛成 0 行）；
 * ② **`null` 差异率不许画成 0%**：标准应发生额为 0 时数学上不成立，显示「—」（0% 会被读成「没有差异」）；
 * ③ **尾差不许被 2 位金额格式抹掉**：`0.0001` 的尾差要原样显示（它是对平用的那一分钱）；
 * ④ **三要素固定料 / 工 / 费顺序**，后端漏一行也要补占位而**不**补 0；
 * ⑤ **合计直接取后端 `totals`**（服务端分页下前端不累加：拿当前页加出来的合计翻页就变）；
 * ⑥ **`cost_ready=false` 的话术**：必须说清「本期没有成本核算批次」+ 下一步去哪，
 *    不能让人以为「差异全是 0」是算坏了。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import type {
  CostVarianceElement,
  CostVarianceRes,
  CostVarianceRow,
  CostVarianceStandardSide,
  CostVarianceTotals,
} from '@/api/business/erp/cost-variance-kinds';
import {
  DIRECTION,
  PAGE_SIZE,
  directionTheme,
  rowStateTheme,
} from '@/api/business/erp/cost-variance-kinds';
import {
  PERIOD_REQUIRED_HINT,
  RATE_UNDEFINED_HINT,
  actualFacts,
  basisTextOf,
  buildQuery,
  completedQtyText,
  elementRows,
  elementsCompactText,
  emptyTextOf,
  exactText,
  hasPeriod,
  metaSuffixOf,
  moneyText,
  noteText,
  qtyText,
  rateText,
  rowElements,
  rowMaterialText,
  rowStateText,
  scopeHintOf,
  specText,
  standardFacts,
  summaryCards,
  unitText,
  varianceFacts,
  versionText,
  type VarianceFilter,
} from '@/views/business/erp/cost-variance/cost-variance-logic';

// ==================== 夹具 ====================

function element(overrides: Partial<CostVarianceElement> = {}): CostVarianceElement {
  return {
    element: 'MATERIAL',
    element_label: '材料',
    std_unit: '12.000000',
    act_unit: '12.500000',
    unit_amount: '0.500000',
    std_amount: '10560.0000',
    act_amount: '15000.0000',
    amount: '4440.0000',
    rate: '0.420454',
    direction: 'UNFAVORABLE',
    direction_label: '不利',
    ...overrides,
  };
}

function totals(overrides: Partial<CostVarianceTotals> = {}): CostVarianceTotals {
  return {
    rows: 3,
    comparable_rows: 2,
    no_standard_rows: 1,
    no_actual_rows: 0,
    completed_qty: '800.0000',
    std_amount: '10560.0000',
    act_amount: '15000.0000',
    amount: '4440.0000',
    rate: '0.420454',
    direction: 'UNFAVORABLE',
    direction_label: '不利',
    no_standard_amount: '300.0000',
    book_amount: '15000.0000',
    rounding_diff: '0.0000',
    elements: [
      element(),
      element({
        element: 'LABOR',
        element_label: '人工',
        std_amount: '4000.0000',
        act_amount: '4320.0000',
        amount: '320.0000',
        rate: '0.08',
        direction: 'UNFAVORABLE',
        direction_label: '不利',
        std_unit: '',
        act_unit: '',
        unit_amount: '',
      }),
      element({
        element: 'OVERHEAD',
        element_label: '制造费用',
        std_amount: '2000.0000',
        act_amount: '2000.0000',
        amount: '0.0000',
        rate: '0.000000',
        direction: 'EVEN',
        direction_label: '持平',
        std_unit: '',
        act_unit: '',
        unit_amount: '',
      }),
    ],
    ...overrides,
  };
}

function standard(overrides: Partial<CostVarianceStandardSide> = {}): CostVarianceStandardSide {
  return {
    id: '11111111-1111-4111-8111-111111111111',
    version_no: 2,
    effective_from: '2026-11-01',
    effective_to: null,
    published_at: '2026-11-02T09:00:00+08:00',
    switch_in_period: false,
    std_material_cost: '12.000000',
    std_labor_cost: '5.000000',
    std_overhead_cost: '2.500000',
    std_total_cost: '19.500000',
    std_labor_hours: '0.1000',
    std_labor_rate: '50.000000',
    ...overrides,
  };
}

function row(overrides: Partial<CostVarianceRow> = {}): CostVarianceRow {
  const standardSide = overrides.standard === undefined ? standard() : overrides.standard;
  return {
    material_id: '22222222-2222-4222-8222-222222222222',
    material_code: 'FG-1001',
    material_name: '成品A',
    material_spec: '10×20',
    state: 'BOTH',
    state_label: '可比',
    standard: standardSide,
    actual: {
      object_count: 1,
      completed_qty: '800.0000',
      book_amount: '15000.0000',
      amount_material: '15000.0000',
      amount_labor: '4320.0000',
      amount_overhead: '2000.0000',
      amount_total: '21320.0000',
      unit_material: '12.500000',
      unit_labor: '5.400000',
      unit_overhead: '2.500000',
      unit_total: '20.400000',
      period_input: '21000.0000',
      wip_end_cost: '0.0000',
    },
    variance: {
      completed_qty: '800.0000',
      std_amount: '10560.0000',
      act_amount: '15000.0000',
      amount: '4440.0000',
      rate: '0.420454',
      direction: 'UNFAVORABLE',
      direction_label: '不利',
      unit_std_amount: '19.500000',
      unit_act_amount: '20.400000',
      unit_amount: '0.900000',
      elements: [element()],
      rounding_diff: '0.0000',
    },
    hint: '',
    ...overrides,
  };
}

function res(overrides: Partial<CostVarianceRes> = {}): CostVarianceRes {
  return {
    period_code: '2026-11',
    period_start: '2026-11-01',
    period_end: '2026-11-30',
    standard_basis: '期间末生效 + 已发布 + 启用的那一版标准成本',
    batch: { id: 'b1', batch_no: 'CS-202611-0001', status: 'SETTLED' },
    cost_ready: true,
    state_filter: 'all',
    keyword: null,
    list: [row()],
    total: 1,
    page: 1,
    page_size: 20,
    totals: totals(),
    note: '差异 = 实际 − 标准，正数 = 不利（超支）。差异**不入账**：本报表只让差异被看见（ARCH-9 D-15）。',
    ...overrides,
  };
}

function filter(overrides: Partial<VarianceFilter> = {}): VarianceFilter {
  return { period_code: '2026-11', keyword: '', state: 'all', ...overrides };
}

// ==================== ① 入参拼装 ====================

test('buildQuery: 合法期间 → 出参带 period_code / state / page / page_size', () => {
  const query = buildQuery(filter({ keyword: '  FG-1001  ', state: 'comparable' }), 2, 50);
  assert.deepEqual(query, {
    period_code: '2026-11',
    state: 'comparable',
    page: 2,
    page_size: 50,
    keyword: 'FG-1001',
  });
});

test('buildQuery: 期间缺失 / 非法 ⇒ null（页面据此不发请求）', () => {
  for (const bad of ['', '   ', '2026-13', '2026-00', '202611', '2026-1', '2026-11-01']) {
    assert.equal(buildQuery(filter({ period_code: bad }), 1), null, `期间「${bad}」必须判非法`);
  }
});

test('buildQuery: 空白关键字整键缺席 / state 空归 all / page 与 pageSize 归一到合法值', () => {
  const blankKeyword = buildQuery(filter({ keyword: '   ' }), 1);
  assert.ok(blankKeyword);
  assert.equal('keyword' in blankKeyword, false, '空白关键字不该发出去');

  const defaults = buildQuery(filter({ state: '' }), 0, 0);
  assert.ok(defaults);
  assert.equal(defaults.state, 'all');
  assert.equal(defaults.page, 1);
  assert.equal(defaults.page_size, PAGE_SIZE);

  // 期间两侧空白要去掉（用户从别处复制过来常带空格）
  const padded = buildQuery(filter({ period_code: ' 2026-02 ' }), 1);
  assert.ok(padded);
  assert.equal(padded.period_code, '2026-02');
});

test('hasPeriod: 只认 yyyy-MM 形状', () => {
  assert.equal(hasPeriod({ period_code: '2026-11' }), true);
  assert.equal(hasPeriod({ period_code: '2026-2' }), false);
  assert.equal(hasPeriod({ period_code: '' }), false);
});

// ==================== ② 差异率 ====================

test('rateText: 比值 → 百分比；null / 空 ⇒ —（不画 0%）', () => {
  assert.equal(rateText('0.150000'), '15.00%');
  assert.equal(rateText('0.000000'), '0.00%');
  assert.equal(rateText('-0.250000'), '-25.00%');
  assert.equal(rateText('0.420454'), '42.05%');
  assert.equal(rateText(null), '—', '标准额为 0 时后端给 null，页面不许显示 0%');
  assert.equal(rateText(undefined), '—');
  assert.equal(rateText(''), '—');
  assert.equal(rateText('abc'), '—');
});

// ==================== ③ 金额 / 数量 / 单价 ====================

test('moneyText: 千分位 + 2 位（后端 4 位小数字符串）', () => {
  assert.equal(moneyText('10560.0000'), '10,560.00');
  assert.equal(moneyText('-180.0000'), '-180.00');
  assert.equal(moneyText('0.0000'), '0.00');
  assert.equal(moneyText(null), '0.00');
});

test('qtyText: 去尾零 + 千分位（数量 / 工时）', () => {
  assert.equal(qtyText('800.0000'), '800');
  assert.equal(qtyText('1234.5000'), '1,234.5');
  assert.equal(qtyText('-1200.0000'), '-1,200');
  assert.equal(qtyText('0.0000'), '0');
  assert.equal(qtyText(''), '—');
});

test('unitText: 单位成本去尾零但保留有效位', () => {
  assert.equal(unitText('12.000000'), '12');
  assert.equal(unitText('17.166666'), '17.166666');
  assert.equal(unitText('0.1000'), '0.1');
  assert.equal(unitText(null), '—');
});

test('exactText: 尾差不被抹成 0（2 位金额格式会把它吃掉）', () => {
  assert.equal(exactText('0.0001'), '0.0001');
  assert.equal(exactText('12345.6000'), '12,345.6');
  assert.equal(moneyText('0.0001'), '0.00', '对照：金额格式确实会把它抹成 0.00');
});

// ==================== ④ 三要素 ====================

test('elementRows: 固定按 料 / 工 / 费 排序（后端乱序也照排）', () => {
  const rows = elementRows([
    element({ element: 'OVERHEAD', element_label: '制造费用' }),
    element({ element: 'MATERIAL', element_label: '材料' }),
    element({ element: 'LABOR', element_label: '人工' }),
  ]);
  assert.deepEqual(
    rows.map((row_) => row_.key),
    ['MATERIAL', 'LABOR', 'OVERHEAD'],
  );
});

test('elementRows: 后端漏一行 ⇒ 补占位且**不**补 0（漏行不能看起来像「没差异」）', () => {
  const rows = elementRows([element({ element: 'MATERIAL' })]);
  assert.equal(rows.length, 3, '三要素永远是三行');
  const overhead = rows[2];
  assert.ok(overhead);
  assert.equal(overhead.key, 'OVERHEAD');
  assert.equal(overhead.label, '制造费用', '后端没给就用兜底中文');
  assert.equal(overhead.stdAmount, '—');
  assert.equal(overhead.actAmount, '—');
  assert.equal(overhead.amount, '—');
  assert.equal(overhead.rate, '—');
  assert.equal(overhead.tone, 'default');
});

test('elementRows: 要素中文优先用后端 element_label；rate=null ⇒ —', () => {
  const rows = elementRows([
    element({ element: 'MATERIAL', element_label: '', rate: null, direction: 'FAVORABLE', direction_label: '' }),
  ]);
  const material = rows[0];
  assert.ok(material);
  assert.equal(material.label, '材料', '后端 label 空时用兜底中文，别把 MATERIAL 糊在界面上');
  assert.equal(material.rate, '—');
  assert.equal(material.directionLabel, '有利', '后端 label 空时用方向兜底中文');
  assert.equal(material.tone, 'success', '有利 = 绿');
});

test('rowElements: 不可比行没有三要素（返回空数组，页面画「没法比」的原因）', () => {
  assert.deepEqual(rowElements(row({ variance: null, state: 'NO_STANDARD', state_label: '缺标准' })), []);
  assert.equal(rowElements(row()).length, 3, '可比行永远是料 / 工 / 费三行（后端只回一行时补占位）');
});

test('elementsCompactText: 可比行给紧凑三要素；不可比行给 —', () => {
  const comparable = row({
    variance: {
      ...row().variance!,
      elements: [
        element({ element: 'MATERIAL', amount: '600.0000' }),
        element({ element: 'LABOR', amount: '-180.0000' }),
        element({ element: 'OVERHEAD', amount: '0.0000' }),
      ],
    },
  });
  assert.equal(elementsCompactText(comparable), '料 600.00 · 工 -180.00 · 费 0.00');
  assert.equal(
    elementsCompactText(row({ variance: null, state: 'NO_STANDARD', state_label: '缺标准' })),
    '—',
  );
});

// ==================== ⑤ 汇总（后端 totals 直用） ====================

test('summaryCards: 6 张卡且合计值**原样**来自后端 totals（不重新累加）', () => {
  const cards = summaryCards(res());
  assert.deepEqual(
    cards.map((card) => card.key),
    ['rows', 'std', 'act', 'amount', 'rate', 'gap'],
  );
  const byKey = new Map(cards.map((card) => [card.key, card]));
  assert.equal(byKey.get('rows')?.value, '3');
  assert.match(byKey.get('rows')?.sub ?? '', /可比 2 · 缺标准 1 · 缺实际 0/);
  assert.equal(byKey.get('std')?.value, '10,560.00');
  assert.equal(byKey.get('act')?.value, '15,000.00');
  assert.equal(byKey.get('amount')?.value, '4,440.00');
  assert.equal(byKey.get('rate')?.value, '42.05%');
  assert.equal(byKey.get('gap')?.value, '300.00');
  assert.equal(byKey.get('amount')?.tone, 'danger', '不利 = 红');
});

test('summaryCards: 差异率 null ⇒ 卡片显示「—」并把原因写在副标题里', () => {
  const cards = summaryCards(
    res({ totals: totals({ rate: null, amount: '0.0000', direction: 'EVEN', direction_label: '持平' }) }),
  );
  const rate = cards.find((card) => card.key === 'rate');
  assert.ok(rate);
  assert.equal(rate.value, '—');
  assert.equal(rate.sub, RATE_UNDEFINED_HINT);
  const amount = cards.find((card) => card.key === 'amount');
  assert.equal(amount?.tone, 'default', '持平不染色');
});

test('summaryCards: 没有数据 ⇒ 不出卡片（页面据此整块不画）', () => {
  assert.deepEqual(summaryCards(null), []);
});

test('metaSuffixOf: 有不可比行时把行数说在「共 N 行」同一个元素里', () => {
  assert.equal(metaSuffixOf(totals()), '（其中 1 行不可比，不进差异合计）');
  assert.equal(
    metaSuffixOf(totals({ no_standard_rows: 0, no_actual_rows: 0 })),
    '',
    '全可比时不啰嗦',
  );
  assert.equal(metaSuffixOf(null), '');
});

// ==================== ⑥ 话术 ====================

test('scopeHintOf: cost_ready=false 时说清原因与下一步；就绪时为空', () => {
  assert.equal(scopeHintOf(res()), '');
  const hint = scopeHintOf(res({ cost_ready: false, batch: null }));
  assert.match(hint, /2026-11/);
  assert.match(hint, /成本核算/);
  assert.match(hint, /cost-period\/calc/, '要给出可执行的接口 / 入口，别只说「请检查」');
  assert.equal(scopeHintOf(null), '');
});

test('basisTextOf: 口径文案 + 基准日（照抄后端，不改写）', () => {
  const text = basisTextOf(res());
  assert.match(text, /期间末生效 \+ 已发布 \+ 启用的那一版标准成本/);
  assert.match(text, /2026-11-30/, '基准日必须写清是哪一天');
  assert.equal(basisTextOf(null), '');
});

test('noteText: 去掉 markdown 强调记号并折叠换行，口径原文不动', () => {
  const text = noteText(res().note);
  assert.equal(text.includes('**'), false, '纯文本 alert 里不许露出星号');
  assert.match(text, /差异 = 实际 − 标准/);
  assert.match(text, /不入账/);
  assert.match(text, /ARCH-9 D-15/);
  assert.equal(noteText(null), '');
});

test('emptyTextOf: 没选期间 / 筛选没命中 / 两边都没有 —— 三种空态话术不同', () => {
  assert.equal(emptyTextOf(null, filter({ period_code: '' })), PERIOD_REQUIRED_HINT);

  const notComparable = emptyTextOf(
    res({ list: [], total: 0, state_filter: 'comparable' }),
    filter({ state: 'comparable' }),
  );
  assert.match(notComparable, /可比/);
  assert.match(notComparable, /换个筛选/);

  const nothing = emptyTextOf(res({ list: [], total: 0 }), filter());
  assert.match(nothing, /标准成本/);
  assert.match(nothing, /成本核算/);
});

// ==================== 行内展开 ====================

test('standardFacts / actualFacts: 缺一侧 ⇒ 空数组（页面画「没有」的原因）', () => {
  assert.deepEqual(standardFacts(row({ standard: null })), []);
  assert.deepEqual(actualFacts(row({ actual: null })), []);

  const stdFacts = standardFacts(row());
  assert.ok(stdFacts.some((fact) => fact.label === '版本' && fact.value.startsWith('v2')));
  assert.ok(stdFacts.some((fact) => fact.label === '单位标准总成本' && fact.value === '19.5'));
  assert.ok(stdFacts.some((fact) => fact.label.includes('工时') && fact.value.includes('0.1 h')));

  const actFacts = actualFacts(row());
  assert.ok(actFacts.some((fact) => fact.label === '完工数量' && fact.value === '800'));
  assert.ok(actFacts.some((fact) => fact.label === '本期投入（参考）'));
});

test('versionText: 期内换版时追加标记（别让人以为「怎么不是最新版」）', () => {
  assert.equal(versionText(standard()), 'v2 · 2026-11-01 ~ 至今');
  assert.match(versionText(standard({ switch_in_period: true })), /期内换版/);
  assert.equal(
    versionText(standard({ effective_to: '2026-11-30' })),
    'v2 · 2026-11-01 ~ 2026-11-30',
  );
});

test('varianceFacts: 单位口径 + 尾差（尾差写明「对平用，不是差异」且不丢位）', () => {
  const facts = varianceFacts({
    ...row().variance!,
    rounding_diff: '0.0001',
  });
  assert.ok(facts.some((fact) => fact.label === '单位差异' && fact.value === '0.9'));
  const rounding = facts.find((fact) => fact.label.includes('尾差'));
  assert.ok(rounding);
  assert.equal(rounding.value, '0.0001');
  assert.match(rounding.label, /不是差异/);
  assert.deepEqual(varianceFacts(null), []);
});

test('completedQtyText: 可比行取 variance，缺标准行取实际侧，都没有 ⇒ —', () => {
  assert.equal(completedQtyText(row()), '800');
  assert.equal(
    completedQtyText(row({ variance: null, state: 'NO_STANDARD', state_label: '缺标准' })),
    '800',
    '缺标准行也要能看见本期做了多少',
  );
  assert.equal(completedQtyText(row({ variance: null, actual: null })), '—');
});

test('rowMaterialText / specText / rowStateText: 物料与状态的兜底形态', () => {
  assert.equal(rowMaterialText(row()), 'FG-1001 成品A');
  assert.equal(specText(row()), '10×20');
  assert.equal(specText(row({ material_spec: null })), '—');
  assert.equal(rowStateText(row({ state_label: '' , state: 'NO_ACTUAL' })), '缺实际');
  // 物料被删（编码 / 名称都没了）⇒ 退回 id 前 8 位，别显示空白
  assert.equal(
    rowMaterialText(row({ material_code: null, material_name: null })),
    '22222222',
  );
});

// ==================== 词表 ====================

test('directionTheme / rowStateTheme: 不利红 / 有利绿 / 缺标准红 / 缺实际黄', () => {
  assert.equal(directionTheme(DIRECTION.UNFAVORABLE), 'danger');
  assert.equal(directionTheme(DIRECTION.FAVORABLE), 'success');
  assert.equal(directionTheme(DIRECTION.EVEN), 'default');
  assert.equal(directionTheme('WHATEVER'), 'default');
  assert.equal(rowStateTheme('BOTH'), 'success');
  assert.equal(rowStateTheme('NO_STANDARD'), 'danger');
  assert.equal(rowStateTheme('NO_ACTUAL'), 'warning');
  assert.equal(rowStateTheme(''), 'default');
});

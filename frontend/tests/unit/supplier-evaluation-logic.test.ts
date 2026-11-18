/**
 * M4-16 单测：供应商评估页面的**纯逻辑**（预览总分 / 权重和 / 期间 / 表单校验 / 查询拼装）。
 *
 * 为什么单独钉这几处：
 *
 * | 钉什么 | 钉不住会怎样 |
 * |--------|--------------|
 * | 预览总分与库层同公式（定点、中点远离零） | 页面预览 79.01、保存后库里 79.00，用户以为系统在改他的数 |
 * | 权重和必须**精确** = 1 | 前端放行 0.9999，后端 400，用户白填一遍 |
 * | 等级阈值逐字复述后端 | 预览说 B、落库是 C |
 * | 空筛选 / 半截期间不进请求 | `?keyword=` 让后端查出 0 行；只给 period_start 查出别的期间 |
 * | 无交期数据不显示成 0 分 | 「0 分」= 全部逾期，与「没有数据」是两件事 |
 *
 * 运行：`pnpm test:unit`（或 `npx tsx --test tests/unit/supplier-evaluation-logic.test.ts`）
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  buildEvaluationQuery,
  buildSavePayload,
  currentQuarterRange,
  emptyFilter,
  emptyForm,
  formErrors,
  gradeTheme,
  lastQuarterRange,
  previewGrade,
  previewTotal,
  quarterRange,
  suggestionText,
  toScaled,
  weightsState,
  weightsSum,
} from '../../src/views/business/erp/supplier-evaluation/supplier-evaluation-logic';

// ==================== 定点小数 ====================

test('toScaled 拒绝超过 4 位小数（不静默截断）', () => {
  assert.equal(toScaled('0.25'), 2500);
  assert.equal(toScaled('80'), 800000);
  assert.equal(toScaled('0.0001'), 1);
  assert.equal(toScaled('0.00001'), null, '第 5 位小数据必须判非法，不许截成 0.0000');
  assert.equal(toScaled('abc'), null);
  assert.equal(toScaled(''), null);
});

test('weightsSum 是定点加法（0.1 + 0.2 不出浮点尾巴）', () => {
  assert.equal(weightsSum({ quality: '0.1', delivery: '0.2', price: '0.3', service: '0.4' }), '1.0000');
  assert.equal(weightsSum({ quality: '0.25' }), '0.2500', '空项不计入');
});

test('weightsState：0.9999 不算 1（后端同样拒）', () => {
  const exactlyOne = weightsState({ quality: '0.25', delivery: '0.25', price: '0.25', service: '0.25' });
  assert.equal(exactlyOne.isOne, true);
  assert.equal(exactlyOne.filled, 4);

  const almost = weightsState({ quality: '0.25', delivery: '0.25', price: '0.25', service: '0.2499' });
  assert.equal(almost.isOne, false);
  assert.equal(almost.sumText, '0.9999');

  const over = weightsState({ quality: '0.4', delivery: '0.3', price: '0.2', service: '0.2' });
  assert.equal(over.isOne, false);
  assert.equal(over.sumText, '1.1000');

  const negative = weightsState({ quality: '1.5', delivery: '-0.5', price: '0', service: '0' });
  assert.equal(negative.sumText, '1.0000', '和确实是 1');
  assert.equal(negative.isOne, false, '但单项越界 —— 与 Weights::is_valid 一致');
  assert.equal(negative.inRange, false);
});

// ==================== 预览总分（与库层生成列同公式） ====================

test('previewTotal 手算对拍：0.4/0.3/0.2/0.1 × 80/90/70/60 = 79.00', () => {
  const total = previewTotal(
    { quality: '0.4', delivery: '0.3', price: '0.2', service: '0.1' },
    { quality: '80', delivery: '90', price: '70', service: '60' },
  );
  assert.equal(total, '79.00');
});

test('previewTotal 四舍五入（中点远离零），与 PG round(numeric,2) 一致', () => {
  // 0.5 × 0.01 = 0.005 → 0.01（浮点下会掉成 0.00）
  assert.equal(
    previewTotal({ quality: '0.5', delivery: '0.5', price: '0', service: '0' }, { quality: '0.01', delivery: '0', price: '0', service: '0' }),
    '0.01',
  );
  // 0.25 × 99.99 = 24.9975 → 25.00
  assert.equal(
    previewTotal(
      { quality: '0.25', delivery: '0.25', price: '0.25', service: '0.25' },
      { quality: '99.99', delivery: '0', price: '0', service: '0' },
    ),
    '25.00',
  );
  // 全 100 → 100.00；全 0 → 0.00
  const full = { quality: '0.25', delivery: '0.25', price: '0.25', service: '0.25' };
  assert.equal(previewTotal(full, { quality: '100', delivery: '100', price: '100', service: '100' }), '100.00');
  assert.equal(previewTotal(full, { quality: '0', delivery: '0', price: '0', service: '0' }), '0.00');
});

test('previewTotal 任一项空 / 非法 → null（不显示一个错的数）', () => {
  assert.equal(
    previewTotal({ quality: '0.25', delivery: '0.25', price: '0.25', service: '0.25' }, { quality: '80', delivery: '', price: '70', service: '60' }),
    null,
  );
});

test('previewGrade 逐点对拍阈值（含下界）：90/80/70 都是新等级的第一点', () => {
  assert.equal(previewGrade('100.00'), 'A');
  assert.equal(previewGrade('90.00'), 'A');
  assert.equal(previewGrade('89.99'), 'B');
  assert.equal(previewGrade('80.00'), 'B');
  assert.equal(previewGrade('79.99'), 'C');
  assert.equal(previewGrade('70.00'), 'C');
  assert.equal(previewGrade('69.99'), 'D');
  assert.equal(previewGrade('0.00'), 'D');
  assert.equal(previewGrade(null), null);
});

// ==================== 期间 ====================

test('quarterRange / currentQuarterRange / lastQuarterRange 的边界', () => {
  assert.deepEqual(quarterRange(2026, 1), { start: '2026-01-01', end: '2026-03-31' });
  assert.deepEqual(quarterRange(2026, 2), { start: '2026-04-01', end: '2026-06-30' });
  assert.deepEqual(quarterRange(2024, 1), { start: '2024-01-01', end: '2024-03-31' }, '闰年 Q1 仍是 3-31');
  // 2026-11-18 属于 Q4；上季度 = Q3
  assert.deepEqual(currentQuarterRange(new Date(2026, 10, 18)), { start: '2026-10-01', end: '2026-12-31' });
  assert.deepEqual(lastQuarterRange(new Date(2026, 10, 18)), { start: '2026-07-01', end: '2026-09-30' });
  // 跨年：Q1 的上季度 = 去年 Q4
  assert.deepEqual(lastQuarterRange(new Date(2026, 0, 5)), { start: '2025-10-01', end: '2025-12-31' });
});

// ==================== 表单校验 ====================

function validForm() {
  const form = emptyForm(new Date(2026, 10, 18));
  form.supplier_id = '0193f0e1-0000-7000-8000-000000000001';
  form.scores = { quality: '80', delivery: '90', price: '70', service: '60' };
  return form;
}

test('formErrors：合法表单 0 错误', () => {
  assert.deepEqual(formErrors(validForm()), []);
});

test('formErrors：权重和 ≠ 1 / 缺分数 / 期间倒置 / 评估日早于期间起', () => {
  const bad = validForm();
  bad.weights = { quality: '0.4', delivery: '0.3', price: '0.2', service: '0.2' };
  bad.scores.price = '';
  bad.eval_date = '2026-09-01';
  const errors = formErrors(bad);
  assert.ok(errors.some((item) => item.includes('之和必须精确等于 1')));
  assert.ok(errors.some((item) => item.includes('价格得分必填')));
  assert.ok(errors.some((item) => item.includes('早于评估期间起点')));

  const inverted = validForm();
  inverted.period_start = '2026-12-31';
  inverted.period_end = '2026-10-01';
  assert.ok(formErrors(inverted).some((item) => item.includes('晚于止')));
});

test('formErrors：分数越界 0–100', () => {
  const over = validForm();
  over.scores.quality = '100.01';
  assert.ok(formErrors(over).some((item) => item.includes('0–100')));
});

// ==================== 入参拼装 ====================

test('buildEvaluationQuery：空串整键缺席、期间成对出现', () => {
  const query = buildEvaluationQuery(emptyFilter(), 2, 20);
  assert.deepEqual(query, { page: 2, page_size: 20 });

  const partial = buildEvaluationQuery({ ...emptyFilter(), keyword: '  SEV  ', period_start: '2026-01-01' }, 1, 20);
  assert.equal(partial.keyword, 'SEV', 'trim 后发出');
  assert.equal('period_start' in partial, false, '只给起点不发（页面语义是整段期间）');

  const full = buildEvaluationQuery(
    { keyword: '', grade: 'A', period_start: '2026-01-01', period_end: '2026-03-31' },
    1,
    50,
  );
  assert.deepEqual(full, { page: 1, page_size: 50, grade: 'A', period_start: '2026-01-01', period_end: '2026-03-31' });
});

test('buildSavePayload：没有 total_score / grade（生成列与阈值都在后端）', () => {
  const payload = buildSavePayload(validForm());
  assert.equal('total_score' in payload, false);
  assert.equal('grade' in payload, false);
  assert.equal('eval_date' in payload, false, '空评估日不发（后端按北京时间当天填）');
  assert.equal(payload.quality_weight, '0.25');
  assert.equal(payload.remark, undefined, '空备注不发');

  const withRemark = validForm();
  withRemark.remark = ' 交期两次延误 ';
  withRemark.eval_date = '2026-11-20';
  const filled = buildSavePayload(withRemark);
  assert.equal(filled.remark, '交期两次延误');
  assert.equal(filled.eval_date, '2026-11-20');
});

// ==================== 展示小工具 ====================

test('gradeTheme 五态（含未评级）', () => {
  assert.equal(gradeTheme('A'), 'success');
  assert.equal(gradeTheme('D'), 'danger');
  assert.equal(gradeTheme(null), 'default');
});

test('suggestionText：没有交期数据时给口径说明，不许显示成 0 分', () => {
  const empty = suggestionText({
    docs: 0,
    late_docs: 0,
    total_qty: '0',
    on_time_rate: null,
    suggested_score: null,
    note: '该期间没有可判准时的采购入库，交期维度只能人工打分。',
  });
  assert.ok(empty.includes('只能人工打分'));
  assert.ok(!empty.includes('0%'));

  const usable = suggestionText({
    docs: 3,
    late_docs: 1,
    total_qty: '120.00',
    on_time_rate: '91.67',
    suggested_score: '91.67',
    note: 'x',
  });
  assert.ok(usable.includes('91.67%'));
  assert.ok(usable.includes('3 张入库单里 1 张含逾期行'));
});

/**
 * 标准成本维护页纯逻辑单测（M5-09 · 标准成本「维护页 CRUD + 批量导入」）。
 *
 * 重点钉六件事（都是「算错 / 发错就是白干或把标准写错」的地方）：
 *
 * ① **定点算数**：料 = Σ round(用量×单价,6)、工 = round(工时×费率,6)、总 = 料+工+费，
 *    与后端 `StandardSpec::amounts` 和库层 CHECK 同口径（本文件的期望值直接取自
 *    M5-08 真库用例的同一组数字：17.166666 / 77.777778 / 107.290122）；
 * ② **半远离零舍入**：`77.7777775 → 77.777778`（浮点 `toFixed` 会得 …777，因为
 *    `77.7777775` 在 IEEE754 里其实是 `77.77777749999…`）；
 * ③ **留空 ≠ 0 的分界**：卡头三要素留空合法（= 0），明细用量 / 单价留空是错；
 * ④ **期间 ⇄ 日期**：整期写法只发 `period_code`，自定义窗口才发两个日期（同时发会被
 *    后端判「矛盾」400）；
 * ⑤ **已生效版本锁改**：`effective_from < 今天` 时 `toUpdatePayload` **只发**
 *    停止日 / 状态 / 备注（后端 66109 的判据 + 「重算金额必须等于库金额」那道闸）；
 * ⑥ **导入结果话术**：「全有或全无」失败时必须说清「一条都没入库」+ 逐行原因。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import type { StandardDetailRes, StandardImportResult, StandardRow } from '@/api/business/erp/cost-standard-kinds';
import { statusLabel, statusTheme } from '@/api/business/erp/cost-standard-kinds';
import {
  AMOUNT_SCALE,
  HOURS_SCALE,
  REMARK_MAX,
  canDelete,
  currentPeriod,
  emptyForm,
  emptyItem,
  formFromDetail,
  importIssueRows,
  importSummaryText,
  importSummaryTheme,
  isDateText,
  isLockedEffective,
  isPeriodText,
  isUnchanged,
  isValid,
  materialLabel,
  materialTypeLabel,
  mulRound,
  periodWindow,
  previewAmounts,
  scaledText,
  toCreatePayload,
  toScaled,
  toUpdatePayload,
  todayText,
  trimZero,
  validateForm,
  windowText,
  type StandardFormState,
} from '@/views/business/erp/cost-standard/cost-standard-logic';

// ==================== 夹具 ====================

function head(overrides: Partial<StandardRow> = {}): StandardRow {
  return {
    id: '11111111-1111-4111-8111-111111111111',
    material_id: '22222222-2222-4222-8222-222222222222',
    material_code: 'FG-1001',
    material_name: '成品A',
    material_type: 'FINISHED',
    material_spec: null,
    version_no: 1,
    effective_from: '2026-11-01',
    effective_to: '2026-11-30',
    status: 1,
    status_label: '生效',
    std_material_cost: '17.166666',
    std_labor_cost: '77.777778',
    std_overhead_cost: '12.345678',
    std_total_cost: '107.290122',
    std_labor_hours: '2.5000',
    std_labor_rate: '31.111111',
    items_count: 2,
    remark: '2026 版标准',
    created_at: '2026-11-01T00:00:00+08:00',
    updated_at: '2026-11-01T00:00:00+08:00',
    ...overrides,
  };
}

function detail(overrides: Partial<StandardDetailRes> = {}): StandardDetailRes {
  return {
    head: head(),
    items: [
      {
        id: 'aaaaaaaa-1111-4111-8111-aaaaaaaaaaaa',
        line_no: 1,
        component_material_id: '33333333-3333-4333-8333-333333333333',
        component_material_code: 'RM-2001',
        component_material_name: '原料1',
        std_qty: '3.000000',
        std_price: '5.500000',
        std_amount: '16.500000',
      },
      {
        id: 'bbbbbbbb-1111-4111-8111-bbbbbbbbbbbb',
        line_no: 2,
        component_material_id: '44444444-4444-4444-8444-444444444444',
        component_material_code: 'RM-2002',
        component_material_name: '原料2',
        std_qty: '2.000000',
        std_price: '0.333333',
        std_amount: '0.666666',
      },
    ],
    ...overrides,
  };
}

function form(overrides: Partial<StandardFormState> = {}): StandardFormState {
  return {
    material_id: '22222222-2222-4222-8222-222222222222',
    period_code: '2026-11',
    custom_window: false,
    effective_from: '',
    effective_to: '',
    clear_effective_to: false,
    std_labor_hours: '2.5',
    std_labor_rate: '31.111111',
    std_overhead_cost: '12.345678',
    status: 1,
    remark: '2026 版标准',
    items: [
      {
        key: 'a',
        component_material_id: '33333333-3333-4333-8333-333333333333',
        std_qty: '3',
        std_price: '5.5',
      },
      {
        key: 'b',
        component_material_id: '44444444-4444-4444-8444-444444444444',
        std_qty: '2',
        std_price: '0.333333',
      },
    ],
    ...overrides,
  };
}

// ==================== ① 定点算数 / ② 舍入 ====================

test('定点换算：6 位小数往返，超精度 / 负数 / 非数字一律 null', () => {
  assert.equal(scaledText(toScaled('3.5', AMOUNT_SCALE)!, AMOUNT_SCALE), '3.500000');
  assert.equal(scaledText(toScaled('2.5', HOURS_SCALE)!, HOURS_SCALE), '2.5000');
  assert.equal(scaledText(toScaled('0', AMOUNT_SCALE)!, AMOUNT_SCALE), '0.000000');
  // 7 位小数超精度 ⇒ null（后端 ensure_decimal 也不静默舍入）
  assert.equal(toScaled('1.1234567', AMOUNT_SCALE), null);
  // 5 位小数在 6 位口径下合法
  assert.equal(toScaled('1.12345', AMOUNT_SCALE), 1123450n);
  assert.equal(toScaled('-1', AMOUNT_SCALE), null);
  assert.equal(toScaled('abc', AMOUNT_SCALE), null);
  assert.equal(toScaled('', AMOUNT_SCALE), null);
  assert.equal(toScaled('1,000', AMOUNT_SCALE), null);
});

test('半远离零舍入：77.7777775 → 77.777778（浮点 toFixed 会得 …777）', () => {
  const hours = toScaled('2.5', HOURS_SCALE)!;
  const rate = toScaled('31.111111', AMOUNT_SCALE)!;
  assert.equal(scaledText(mulRound(hours, HOURS_SCALE, rate, AMOUNT_SCALE, AMOUNT_SCALE), AMOUNT_SCALE), '77.777778');
  // 纯 BigInt：0.0000005 → 0.000001（半数进位）
  assert.equal(mulRound(5n, 7, 1n, 0, 6), 1n);
  // 负数同样半远离零（前端只在显示上用，但口径必须一致）
  assert.equal(mulRound(-5n, 7, 1n, 0, 6), -1n);
});

test('三要素预览与 M5-08 真库用例逐位一致（17.166666 / 77.777778 / 107.290122）', () => {
  const preview = previewAmounts(
    [
      { std_qty: '3', std_price: '5.5' },
      { std_qty: '2', std_price: '0.333333' },
    ],
    '2.5',
    '31.111111',
    '12.345678',
  );
  assert.equal(preview.material, '17.166666');
  assert.equal(preview.labor, '77.777778');
  assert.equal(preview.overhead, '12.345678');
  assert.equal(preview.total, '107.290122', '总 = 料 + 工 + 费（库层 ck_biz_cost_std_sum）');
  assert.equal(preview.valid, true);
});

test('预览：卡头留空 = 0，明细数字非法时 valid=false（预览不可信）', () => {
  const preview = previewAmounts([{ std_qty: '', std_price: '' }], '', '', '');
  assert.equal(preview.material, '0.000000');
  assert.equal(preview.labor, '0.000000');
  assert.equal(preview.total, '0.000000');
  assert.equal(preview.valid, false, '明细留空就是没填完');

  const badRate = previewAmounts([], '2.5', 'abc', '');
  assert.equal(badRate.valid, false);
});

// ==================== 期间 / 日期 ====================

test('期间窗口：期末 = 下月 1 日 − 1 天（含闰年 2 月）', () => {
  assert.deepEqual(periodWindow('2026-11'), { start: '2026-11-01', end: '2026-11-30' });
  assert.deepEqual(periodWindow('2026-02'), { start: '2026-02-01', end: '2026-02-28' });
  assert.deepEqual(periodWindow('2028-02'), { start: '2028-02-01', end: '2028-02-29' });
  assert.deepEqual(periodWindow('2026-12'), { start: '2026-12-01', end: '2026-12-31' });
  assert.equal(periodWindow('2026-13'), null);
  assert.equal(periodWindow('2026-1'), null);
  assert.equal(periodWindow(''), null);
});

test('期间 / 日期形状校验（后端 period_window 同口径：拒 2026-1 / 2026-13 / 26-01）', () => {
  assert.equal(isPeriodText('2026-11'), true);
  assert.equal(isPeriodText(' 2026-11 '), true);
  assert.equal(isPeriodText('2026-1'), false);
  assert.equal(isPeriodText('2026-13'), false);
  assert.equal(isPeriodText('26-01'), false);
  assert.equal(isDateText('2026-11-30'), true);
  assert.equal(isDateText('2026-02-30'), false, '不存在的日期');
  assert.equal(isDateText('2026-11-3'), false);
});

test('默认期间 = 本月；今天按本地时区', () => {
  const now = new Date(2026, 10, 19); // 2026-11-19 本地
  assert.equal(todayText(now), '2026-11-19');
  assert.equal(currentPeriod(now), '2026-11');
});

test('窗口文本 / 物料标称', () => {
  assert.equal(windowText('2026-11-01', '2026-11-30'), '2026-11-01 ~ 2026-11-30');
  assert.equal(windowText('2026-11-01', null), '2026-11-01 ~ 至今');
  assert.equal(windowText(null, null), '—');
  assert.equal(materialLabel(head()), 'FG-1001 成品A');
  assert.equal(materialLabel({ material_id: 'abcdefgh-1234' }), 'abcdefgh');
  assert.equal(materialTypeLabel('SEMI'), '半成品');
  assert.equal(materialTypeLabel('SERVICE'), '服务');
  assert.equal(statusLabel(1), '生效');
  assert.equal(statusTheme(0), 'default');
});

// ==================== ③ 校验 ====================

test('校验：新建缺物料 / 期间非法 / 明细留空 都要报出来', () => {
  const errors = validateForm(
    form({
      material_id: '',
      period_code: '2026-1',
      items: [{ key: 'x', component_material_id: '', std_qty: '', std_price: '' }],
    }),
    true,
  );
  assert.ok(errors.material_id);
  assert.ok(errors.period_code);
  assert.ok(errors.items);
  assert.equal(isValid(errors), false);
  assert.equal(isValid(validateForm(form(), true)), true, '合法表单不应有错');
});

test('校验：留空的三要素合法，填了就必须非负且不超精度', () => {
  assert.equal(isValid(validateForm(form({ std_labor_hours: '', std_labor_rate: '', std_overhead_cost: '' }), true)), true);
  assert.ok(validateForm(form({ std_labor_hours: '1.12345' }), true).std_labor_hours, '工时最多 4 位小数');
  assert.ok(validateForm(form({ std_labor_rate: '-1' }), true).std_labor_rate, '费率不能为负');
  assert.ok(validateForm(form({ std_overhead_cost: '1.1234567' }), true).std_overhead_cost, '费用最多 6 位小数');
});

test('校验：明细用量必须 > 0、单价 ≥ 0、同一物料不许重复', () => {
  const zeroQty = validateForm(
    form({ items: [{ key: 'a', component_material_id: 'm1', std_qty: '0', std_price: '1' }] }),
    true,
  );
  assert.ok(zeroQty.items?.includes('标准用量必须 > 0'), zeroQty.items);

  const negativePrice = validateForm(
    form({ items: [{ key: 'a', component_material_id: 'm1', std_qty: '1', std_price: '-1' }] }),
    true,
  );
  assert.ok(negativePrice.items?.includes('标准单价必须 ≥ 0'), negativePrice.items);

  const duplicate = validateForm(
    form({
      items: [
        { key: 'a', component_material_id: 'm1', std_qty: '1', std_price: '1' },
        { key: 'b', component_material_id: 'm1', std_qty: '2', std_price: '1' },
      ],
    }),
    true,
  );
  assert.ok(duplicate.items?.includes('第 2 行'), duplicate.items);
  assert.ok(duplicate.items?.includes('只能出现一次'), duplicate.items);
});

test('校验：备注超 200 字报错；自定义窗口的停止日不能早于生效日', () => {
  assert.ok(validateForm(form({ remark: '备'.repeat(REMARK_MAX + 1) }), true).remark);

  const badWindow = validateForm(
    form({ custom_window: true, effective_from: '2026-11-10', effective_to: '2026-11-01' }),
    false,
  );
  assert.ok(badWindow.effective_to?.includes('不能早于'), badWindow.effective_to);

  const goodWindow = validateForm(
    form({ custom_window: true, effective_from: '2026-11-10', effective_to: '' }),
    false,
  );
  assert.equal(isValid(goodWindow), true, '停止日留空 = 至今有效');
});

// ==================== ④ 入参拼装 ====================

test('新建入参：整期只发 period_code，自定义才发日期；留空的三要素整键缺席', () => {
  const payload = toCreatePayload(
    form({ std_labor_hours: '', std_labor_rate: '  ', std_overhead_cost: '12.345678', remark: '  ' }),
  );
  assert.equal(payload.period_code, '2026-11');
  assert.equal('effective_from' in payload, false);
  assert.equal('effective_to' in payload, false);
  assert.equal('std_labor_hours' in payload, false, '留空 = 不改口径（后端 None → 0）');
  assert.equal(payload.std_overhead_cost, '12.345678');
  assert.equal('remark' in payload, false, '空备注整键缺席');
  assert.equal(payload.items.length, 2);
  assert.deepEqual(payload.items[0], {
    component_material_id: '33333333-3333-4333-8333-333333333333',
    std_qty: '3',
    std_price: '5.5',
  });
});

test('新建入参：自定义窗口发两个日期、不发 period_code（同时发会被后端判矛盾 400）', () => {
  const payload = toCreatePayload(
    form({ custom_window: true, effective_from: '2026-11-10', effective_to: '2026-12-09' }),
  );
  assert.equal('period_code' in payload, false);
  assert.equal(payload.effective_from, '2026-11-10');
  assert.equal(payload.effective_to, '2026-12-09');
});

test('修改入参：未生效版本可以改口径（物料 / 数字 / 明细都发）', () => {
  const payload = toUpdatePayload(form({ std_labor_rate: '50' }), {
    locked: false,
    originalEffectiveTo: '2026-11-30',
  });
  assert.equal(payload.material_id, '22222222-2222-4222-8222-222222222222');
  assert.equal(payload.std_labor_rate, '50');
  assert.equal(payload.items?.length, 2);
  assert.equal(payload.period_code, '2026-11');
  assert.equal('effective_to' in payload, false, '停止日没改就不发');
});

test('修改入参：已生效版本只发停止日 / 状态 / 备注（不发数字与明细）', () => {
  const payload = toUpdatePayload(
    form({ std_labor_rate: '999', effective_to: '2026-11-15' }),
    { locked: true, originalEffectiveTo: '2026-11-30' },
  );
  assert.equal('material_id' in payload, false);
  assert.equal('std_labor_rate' in payload, false, '锁改时绝不发数字（后端会判静默重算历史金额）');
  assert.equal('items' in payload, false);
  assert.equal('period_code' in payload, false);
  assert.equal(payload.effective_to, '2026-11-15', '停止日可以改');
  assert.equal(payload.status, 1);
  assert.equal(payload.remark, '2026 版标准');
});

test('修改入参：把停止日清空 → clear_effective_to（传空串表达不了「至今有效」）', () => {
  const payload = toUpdatePayload(form({ custom_window: true, effective_to: '' }), {
    locked: true,
    originalEffectiveTo: '2026-11-30',
  });
  assert.equal(payload.clear_effective_to, true);
  assert.equal('effective_to' in payload, false);
});

// ==================== ⑤ 锁改判据 ====================

test('已生效判据：生效起始日 < 今天才锁；等于今天还能改', () => {
  const today = '2026-11-19';
  assert.equal(isLockedEffective({ effective_from: '2026-11-01' }, today), true);
  assert.equal(isLockedEffective({ effective_from: '2026-11-19' }, today), false, '今天开始生效的历史还没发生');
  assert.equal(isLockedEffective({ effective_from: '2026-12-01' }, today), false);
  assert.equal(canDelete({ effective_from: '2026-11-19' }, today), true);
  assert.equal(canDelete({ effective_from: '2026-11-18' }, today), false);
});

// ==================== 表单回填 ====================

test('详情回填：整期窗口识别、去掉尾随 0、明细带 id 作 key', () => {
  const state = formFromDetail(detail());
  assert.equal(state.custom_window, false, '2026-11-01 ~ 2026-11-30 正好是整期');
  assert.equal(state.period_code, '2026-11');
  assert.equal(state.std_labor_hours, '2.5');
  assert.equal(state.std_labor_rate, '31.111111');
  assert.equal(state.items.length, 2);
  assert.equal(state.items[0].std_qty, '3');
  assert.equal(state.items[1].std_price, '0.333333');
  assert.equal(state.clear_effective_to, false);
});

test('详情回填：非整期窗口走自定义；停止日 null ⇒ clear_effective_to', () => {
  const state = formFromDetail(
    detail({ head: head({ effective_from: '2026-11-10', effective_to: null }) }),
  );
  assert.equal(state.custom_window, true);
  assert.equal(state.effective_from, '2026-11-10');
  assert.equal(state.effective_to, '');
  assert.equal(state.clear_effective_to, true);
});

test('「没有改动」判定：回填后原样提交算没改，动一个数就算改了', () => {
  const original = detail();
  const state = formFromDetail(original);
  assert.equal(isUnchanged(state, original), true);
  assert.equal(isUnchanged({ ...state, std_labor_rate: '50' }, original), false);
  assert.equal(isUnchanged({ ...state, remark: '改个备注' }, original), false);
  // 数值文本差异不算改动（2.50 与 2.5 是同一个数）
  assert.equal(isUnchanged({ ...state, std_labor_hours: '2.50' }, original), true);
});

test('trimZero：去掉尾随 0，但整数不动', () => {
  assert.equal(trimZero('2.5000'), '2.5');
  assert.equal(trimZero('3.000000'), '3');
  assert.equal(trimZero('0.000000'), '0');
  assert.equal(trimZero('12.345678'), '12.345678');
  assert.equal(trimZero(''), '');
});

test('空表单：默认本月整期、清空明细；emptyItem 的 key 递增且唯一', () => {
  const state = emptyForm(new Date(2026, 10, 19));
  assert.equal(state.period_code, '2026-11');
  assert.equal(state.custom_window, false);
  assert.equal(state.items.length, 0);
  assert.equal(state.status, 1);
  const a = emptyItem();
  const b = emptyItem();
  assert.notEqual(a.key, b.key);
});

// ==================== ⑥ 导入结果 ====================

test('导入结果话术：失败时说清「整批未入库」并按行去重', () => {
  const result: StandardImportResult = {
    success: 0,
    failed: 2,
    versions_created: 0,
    rows: [
      { line: 3, column: '物料编码', message: '物料「X」不存在' },
      { line: 3, column: '期间', message: '期间必须是 yyyy-MM' },
      { line: 5, column: '标准用量', message: '标准用量必须大于 0' },
    ],
  };
  const text = importSummaryText(result);
  assert.ok(text.includes('整批未入库'), text);
  assert.ok(text.includes('一条都没写入'), text);
  assert.ok(text.includes('2 行'), text, '按行去重：第 3 行两条错只算一行');
  assert.equal(importSummaryTheme(result), 'error');

  const rows = importIssueRows(result);
  assert.deepEqual(rows.map((row) => row.line), [3, 3, 5], '按行号升序');
  assert.equal(new Set(rows.map((row) => row.key)).size, 3, 'key 唯一（v-for 用）');
});

test('导入结果话术：成功时说清「行数 → 版本数」', () => {
  const result: StandardImportResult = { success: 9, failed: 0, versions_created: 3, rows: [] };
  const text = importSummaryText(result);
  assert.ok(text.includes('9 行'), text);
  assert.ok(text.includes('3 版'), text);
  assert.equal(importSummaryTheme(result), 'success');
  assert.equal(importIssueRows(result).length, 0);
});

/**
 * 成本对象页纯逻辑单测（COST-B1-11 / ARCH-9 §6.2.1 · §4.1）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 为什么测 `.ts` 不测 `.vue`：「哪个状态能编辑 / 能关批」「必填缺哪个」
 * 「编辑只带真改过的字段」「带在制关批要不要二次确认」全在 `cost-object-logic.ts`；
 * `.vue` 只负责画。这些口径一旦漂移（把 `v-if` 改成 `:disabled`、把 PUT 整表回传），
 * `vue-tsc` 抓不到 —— 只有把判据写成函数才钉得住。
 *
 * 断言口径全部与后端逐字对齐：
 * - 状态字面量：`ck_biz_cost_object_status` = `OPEN | CLOSED | CANCELLED`
 * - 投料方式：`ck_biz_cost_object_input` = `ONE_TIME | AS_PROGRESS`（一期只实现 ONE_TIME）
 * - 权限码：`V0378__cost_menu_perm.up.sql` / `cost_object_handler.rs::COST_OBJECT_PERMISSIONS`
 * - 关批业务码：`error_code::ERP_COST_OBJECT_CLOSE_BLOCKED = 66055`
 * - 关批 5 条前置分类：`cost_object_dto.rs::{BLOCK_DRAFT_LABOR,…}`
 * - 列表筛选：`CostObjectPageQuery{start_from,start_to}`（**没有** period_code）
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  CLOSE_BLOCK_CODES,
  CLOSE_BLOCK_HINTS,
  COST_OBJECT_CLOSE_BLOCKED_CODE,
  COST_OBJECT_STATUSES,
  MATERIAL_INPUT_MODES,
  type CostObjectDetail,
} from '@/api/business/erp/cost-object-kinds';
import {
  COST_OBJECT_PERMISSIONS,
  canCloseObject,
  canCloseRow,
  canEditObject,
  closeErrorText,
  closeSummaryOf,
  emptyCostObjectForm,
  errorCount,
  formFromDetail,
  formatQty,
  hasCollection,
  inputModeLabel,
  isDateString,
  isEmptyPayload,
  isPositiveQty,
  isZeroQty,
  monthRange,
  requiresForceWip,
  statusLabel,
  statusTheme,
  toClosePayload,
  toCreatePayload,
  toUpdatePayload,
  validateCostObjectForm,
} from '@/views/business/erp/cost-object/cost-object-logic';

/** 造一个完整详情（只覆盖页面真正用到的字段，其余给中性值） */
function detailOf(overrides: Partial<CostObjectDetail> = {}): CostObjectDetail {
  return {
    id: '11111111-1111-4111-8111-111111111111',
    object_no: 'CB-20261101-000001',
    object_type: 'BATCH',
    product_id: '22222222-2222-4222-8222-222222222222',
    product_name: '钢轴',
    product_code: 'MAT-0001',
    lot_no: 'LOT-A',
    target_warehouse_id: '33333333-3333-4333-8333-333333333333',
    target_warehouse_name: '成品仓',
    target_location_id: null,
    target_location_name: null,
    plan_qty: '100.0000',
    completed_qty: '60.0000',
    wip_qty: '0.0000',
    completion_pct: '0.00',
    material_input_mode: 'ONE_TIME',
    source_doc_no: 'SO-2026-0001',
    customer_id: null,
    customer_name: null,
    workshop_dept_id: '44444444-4444-4444-8444-444444444444',
    workshop_dept_name: '一车间',
    start_date: '2026-11-01',
    due_date: '2026-11-30',
    status: 'OPEN',
    closed_at: null,
    closed_by: null,
    remark: null,
    created_at: '2026-11-01T08:00:00+08:00',
    updated_at: '2026-11-01T08:00:00+08:00',
    collect_summary: { mat: '100.0000', labor: '20.0000', oh: '10.0000', total: '130.0000' },
    period_items: [],
    next_statuses: ['CLOSED', 'CANCELLED'],
    legal_edges: [
      ['OPEN', 'CLOSED'],
      ['OPEN', 'CANCELLED'],
    ],
    ...overrides,
  };
}

/** 填满必填项的表单 */
function validForm() {
  const form = emptyCostObjectForm('2026-11-01');
  form.product_id = '22222222-2222-4222-8222-222222222222';
  form.target_warehouse_id = '33333333-3333-4333-8333-333333333333';
  form.workshop_dept_id = '44444444-4444-4444-8444-444444444444';
  form.plan_qty = '100';
  return form;
}

// ==================== 词表 / 权限码 ====================

test('权限码与 V0378 种子逐字一致，四个码互不相同', () => {
  assert.equal(COST_OBJECT_PERMISSIONS.list, 'business:erp:cost-object:list');
  assert.equal(COST_OBJECT_PERMISSIONS.create, 'business:erp:cost-object:create');
  assert.equal(COST_OBJECT_PERMISSIONS.update, 'business:erp:cost-object:update');
  assert.equal(COST_OBJECT_PERMISSIONS.close, 'business:erp:cost-object:close');
  const codes = Object.values(COST_OBJECT_PERMISSIONS);
  assert.equal(new Set(codes).size, 4, '能看 ≠ 能建 ≠ 能改 ≠ 能关批');
});

test('状态 / 投料方式词表与 DDL CHECK 逐字一致', () => {
  assert.deepEqual([...COST_OBJECT_STATUSES], ['OPEN', 'CLOSED', 'CANCELLED']);
  assert.deepEqual([...MATERIAL_INPUT_MODES], ['ONE_TIME', 'AS_PROGRESS']);
  assert.equal(statusLabel('OPEN'), '在产');
  assert.equal(statusLabel('CLOSED'), '已关批');
  assert.equal(statusLabel('CANCELLED'), '已作废');
  // 脏值不吞掉：原样显示，现场排查时能看出库里写了什么
  assert.equal(statusLabel('AUDITED'), 'AUDITED');
  assert.equal(statusLabel(null), '—');
  assert.equal(inputModeLabel('ONE_TIME'), '一次投入');
  assert.equal(inputModeLabel('AS_PROGRESS'), '随进度投入');
});

test('状态 → Tag 主题：在产绿 / 已关批灰 / 作废红', () => {
  assert.equal(statusTheme('OPEN'), 'success');
  assert.equal(statusTheme('CLOSED'), 'default');
  assert.equal(statusTheme('CANCELLED'), 'danger');
  assert.equal(statusTheme('WHATEVER'), 'default');
});

test('关批业务码 66055 + 5 条前置分类都有「去哪修」的指引', () => {
  assert.equal(COST_OBJECT_CLOSE_BLOCKED_CODE, 66055);
  assert.deepEqual(
    [...CLOSE_BLOCK_CODES],
    ['DRAFT_LABOR', 'UNCOLLECTED_STOCK_OUT', 'PERIOD_LOCKED', 'UNPOSTED_STOCK_OUT', 'WIP_NOT_ZERO'],
  );
  for (const code of CLOSE_BLOCK_CODES) {
    assert.ok(CLOSE_BLOCK_HINTS[code].length > 4, `${code} 要有指引`);
  }
});

// ==================== 期间筛选 ====================

test('开工期间 → start_from / start_to（月边界算准，含闰年 2 月）', () => {
  assert.deepEqual(monthRange('2026-11'), {
    start_from: '2026-11-01',
    start_to: '2026-11-30',
  });
  assert.deepEqual(monthRange('2026-12'), {
    start_from: '2026-12-01',
    start_to: '2026-12-31',
  });
  assert.deepEqual(monthRange('2026-02'), { start_from: '2026-02-01', start_to: '2026-02-28' });
  assert.deepEqual(monthRange('2024-02'), { start_from: '2024-02-01', start_to: '2024-02-29' });
});

test('非法期间不带日期条件（清空筛选框不该让页面炸）', () => {
  for (const bad of ['', '  ', '2026-13', '2026-1', '202611', 'abc', null, undefined]) {
    assert.deepEqual(monthRange(bad), {}, `月份 ${String(bad)} 不该产生区间`);
  }
});

// ==================== 表单校验 ====================

test('空表单：5 个必填项各自报错（产出品 / 车间 / 目标仓 / 数量 / 开工日）', () => {
  const form = emptyCostObjectForm('2026-11-01');
  // 空表单的开工日默认填今天，这里显式清空才能验到「开工日期必填」
  form.start_date = '';
  const errors = validateCostObjectForm(form);
  assert.equal(errorCount(errors), 5);
  for (const key of [
    'product_id',
    'workshop_dept_id',
    'target_warehouse_id',
    'plan_qty',
    'start_date',
  ] as const) {
    assert.ok(errors[key], `${key} 必填`);
  }
});

test('填满必填项 → 0 错误', () => {
  const errors = validateCostObjectForm(validForm());
  assert.deepEqual(errors, {});
  assert.equal(errorCount(errors), 0);
});

test('计划数量：0 / 负数 / 非数字 / 超 4 位小数 全部拦下', () => {
  const base = validForm();
  const cases: Array<[string, string]> = [
    ['', '必填'],
    ['0', '大于 0'],
    ['0.0000', '大于 0'],
    ['-5', '不能为负'],
    ['abc', '小数'],
    ['1.23456', '小数'],
  ];
  for (const [qty, expect] of cases) {
    const errors = validateCostObjectForm({ ...base, plan_qty: qty });
    assert.ok(errors.plan_qty, `数量「${qty}」应报错`);
    assert.ok(
      errors.plan_qty?.includes(expect),
      `数量「${qty}」文案应含「${expect}」，实得「${errors.plan_qty}」`,
    );
  }
  // 合法：整数 / 4 位小数
  assert.equal(validateCostObjectForm({ ...base, plan_qty: '12.5' }).plan_qty, undefined);
  assert.equal(validateCostObjectForm({ ...base, plan_qty: '12.0001' }).plan_qty, undefined);
});

test('日期：格式与先后（与后端 ck_biz_cost_object_dates 同口径）', () => {
  assert.equal(isDateString('2026-11-01'), true);
  assert.equal(isDateString('2026-02-30'), false, '2 月没有 30 号');
  assert.equal(isDateString('2026-13-01'), false);
  assert.equal(isDateString('2026-1-1'), false);
  assert.equal(isDateString(''), false);

  const base = validForm();
  assert.ok(
    validateCostObjectForm({ ...base, due_date: '2026-10-31' }).due_date,
    '计划完工早于开工要报错',
  );
  assert.equal(
    validateCostObjectForm({ ...base, due_date: '2026-11-01' }).due_date,
    undefined,
    '同一天合法',
  );
  assert.equal(
    validateCostObjectForm({ ...base, due_date: '' }).due_date,
    undefined,
    '可空',
  );
  assert.ok(validateCostObjectForm({ ...base, start_date: '' }).start_date);
});

test('已有归集时产出品只读 → 校验不再报它（避免只读字段标红）', () => {
  const form = validForm();
  form.product_id = '';
  assert.ok(validateCostObjectForm(form).product_id);
  assert.equal(validateCostObjectForm(form, { collected: true }).product_id, undefined);
});

// ==================== 建单 payload ====================

test('建单 payload：trim + 可空字段空串一律不发 + 投料方式默认一次投入', () => {
  const form = validForm();
  form.plan_qty = ' 100 ';
  form.due_date = ' ';
  form.source_doc_no = ' SO-0001 ';
  form.lot_no = '';
  form.target_location_id = '';
  form.remark = '   ';
  const payload = toCreatePayload(form);
  assert.equal(payload.plan_qty, '100');
  assert.equal(payload.source_doc_no, 'SO-0001', 'trim 后带上');
  assert.equal(payload.material_input_mode, 'ONE_TIME');
  for (const key of [
    'due_date',
    'lot_no',
    'target_location_id',
    'remark',
  ] as const) {
    assert.equal(key in payload, false, `${key} 空串不该出现在 payload 里`);
  }
});

// ==================== 编辑 payload（部分更新 + 三态） ====================

test('编辑：什么都没改 → 空 payload（页面提示「没有改动」且不发请求）', () => {
  const origin = detailOf();
  const form = formFromDetail(origin);
  const payload = toUpdatePayload(origin, form);
  assert.deepEqual(payload, {});
  assert.equal(isEmptyPayload(payload), true);
});

test('编辑：数量 100.0000 vs 表单 100 视为未改（避免无意义写入）', () => {
  const origin = detailOf({ plan_qty: '100.0000' });
  const form = formFromDetail(origin);
  assert.equal(form.plan_qty, '100');
  assert.deepEqual(toUpdatePayload(origin, form), {});
});

test('编辑：可空字段被清空 → 显式 null（不是「不改」）', () => {
  const origin = detailOf({ due_date: '2026-11-30', lot_no: 'LOT-A', remark: '首件' });
  const form = formFromDetail(origin);
  form.due_date = '';
  form.lot_no = '';
  const payload = toUpdatePayload(origin, form);
  assert.equal(payload.due_date, null, '清空完工日期要写 null');
  assert.equal(payload.lot_no, null, '清空批号要写 null');
  assert.equal('remark' in payload, false, '备注没动就不带');
});

test('编辑：原本为空、现在仍为空 → 不带该字段', () => {
  const origin = detailOf({ customer_id: null, source_doc_no: null });
  const form = formFromDetail(origin);
  const payload = toUpdatePayload(origin, form);
  assert.equal('customer_id' in payload, false);
  assert.equal('source_doc_no' in payload, false);
});

test('编辑：真改的字段才进 payload（含车间 / 日期 / 备注）', () => {
  const origin = detailOf();
  const form = formFromDetail(origin);
  form.workshop_dept_id = '55555555-5555-4555-8555-555555555555';
  form.start_date = '2026-11-05';
  form.remark = '改期';
  const payload = toUpdatePayload(origin, form);
  assert.deepEqual(payload, {
    workshop_dept_id: '55555555-5555-4555-8555-555555555555',
    start_date: '2026-11-05',
    remark: '改期',
  });
});

// ==================== 按钮态 ====================

test('只有 OPEN 能编辑 / 能关批（CLOSED 不可逆、CANCELLED 是终态）', () => {
  assert.equal(canEditObject('OPEN'), true);
  assert.equal(canEditObject('CLOSED'), false);
  assert.equal(canEditObject('CANCELLED'), false);
  assert.equal(canCloseRow({ status: 'OPEN' }), true);
  assert.equal(canCloseRow({ status: 'CLOSED' }), false);
  assert.equal(canCloseObject(detailOf()), true);
  assert.equal(canCloseObject(detailOf({ status: 'CLOSED' })), false);
  assert.equal(canCloseObject(null), false);
});

test('有效边里没有 OPEN→CLOSED 时不给关批按钮（点了必 409）', () => {
  assert.equal(canCloseObject(detailOf({ next_statuses: ['CANCELLED'] })), false);
  // 后端没返回有效边（老版本 / 异常）时不硬拦，按状态放行
  assert.equal(canCloseObject(detailOf({ next_statuses: [] })), true);
});

test('归集判定：total 非 0 即视为有归集', () => {
  assert.equal(hasCollection(detailOf()), true);
  assert.equal(
    hasCollection(detailOf({ collect_summary: { mat: '0', labor: '0', oh: '0', total: '0' } })),
    false,
  );
  assert.equal(hasCollection(null), false);
});

// ==================== 关批 ====================

test('在制数量不为 0 → 必须「带在制关批」（§4.1 前置 ⑤）', () => {
  assert.equal(requiresForceWip(detailOf({ wip_qty: '0.0000' })), false);
  // NUMERIC(18,4) 的分辨率是 0.0001，比它小的值库层就是 0 —— 不逼用户确认
  assert.equal(requiresForceWip(detailOf({ wip_qty: '0.00001' })), false);
  assert.equal(requiresForceWip(detailOf({ wip_qty: '0.0001' })), true);
  assert.equal(requiresForceWip(detailOf({ wip_qty: '12.0000' })), true);
  assert.equal(requiresForceWip(null), false);
});

test('关批确认摘要：对象号 / 产出品+批号 / 归集成本 / 在制数量 / 不可逆提示', () => {
  const summary = closeSummaryOf(detailOf({ wip_qty: '12.0000' }));
  assert.equal(summary.object_no, 'CB-20261101-000001');
  assert.equal(summary.product, '钢轴 / LOT-A');
  assert.equal(summary.collect_total, '130.0000');
  assert.equal(summary.wip_qty, '12');
  assert.equal(summary.need_force_wip, true);
  assert.ok(summary.irreversible_hint.includes('不可回'));
});

test('关批 payload：勾了才带 force_wip；备注 trim 后为空不带', () => {
  assert.deepEqual(toClosePayload(), {});
  assert.deepEqual(toClosePayload({ forceWip: false, remark: '  ' }), {});
  assert.deepEqual(toClosePayload({ forceWip: true }), { force_wip: true });
  assert.deepEqual(toClosePayload({ remark: ' 带在制关批 ' }), { remark: '带在制关批' });
});

test('关批错误文案：66055 保留后端逐条原因，并补「去哪修」指引', () => {
  const message =
    '成本对象 CB-20261101-000001 不能关批：[DRAFT_LABOR] 还有 3 条草稿工时台账没确认；' +
    '[WIP_NOT_ZERO] 期末在制数量是 12（不为 0）';
  const text = closeErrorText({ code: 66055, message });
  assert.ok(text.startsWith(message), '后端原文必须保留（它是逐条原因）');
  assert.ok(text.includes(CLOSE_BLOCK_HINTS.DRAFT_LABOR), '补工时台账指引');
  assert.ok(text.includes(CLOSE_BLOCK_HINTS.WIP_NOT_ZERO), '补带在制关批指引');
  // 普通 409（状态不是 OPEN）原样返回，不硬加指引
  const plain = closeErrorText({ code: 60009, message: '成本对象当前是「已关批」' });
  assert.equal(plain, '成本对象当前是「已关批」');
  assert.equal(closeErrorText('boom'), 'boom');
  assert.equal(closeErrorText(undefined), '关批失败，请重试');
});

// ==================== 展示小件 ====================

test('数量展示去掉无意义尾随 0（金额仍走 formatMoney）', () => {
  assert.equal(formatQty('100.0000'), '100');
  assert.equal(formatQty('12.5000'), '12.5');
  assert.equal(formatQty('12.0001'), '12.0001');
  assert.equal(formatQty('0.0000'), '0');
  assert.equal(formatQty('-0.0000'), '0');
  assert.equal(formatQty(''), '0');
  assert.equal(formatQty(null), '0');
});

test('数量正负 / 零判定（字符串安全，不累加浮点）', () => {
  assert.equal(isPositiveQty('0.0001'), true);
  assert.equal(isPositiveQty('0'), false);
  assert.equal(isPositiveQty('-1'), false);
  assert.equal(isPositiveQty('abc'), false);
  assert.equal(isZeroQty('0.0000'), true);
  assert.equal(isZeroQty('0.00004'), true, '四位小数口径内视为 0');
  assert.equal(isZeroQty('0.0001'), false);
});

test('详情 → 表单回填：可空字段落成空串，数量按展示口径', () => {
  const form = formFromDetail(detailOf({ target_location_id: null, due_date: null }));
  assert.equal(form.target_location_id, '');
  assert.equal(form.due_date, '');
  assert.equal(form.plan_qty, '100');
  assert.equal(form.material_input_mode, 'ONE_TIME');
  assert.equal(form.start_date, '2026-11-01');
});

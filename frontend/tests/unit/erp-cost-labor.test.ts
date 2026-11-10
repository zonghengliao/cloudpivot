/**
 * 工时台账页纯逻辑单测（COST-B2-13 / ARCH-9 §6.2.4 · §5.2 #10–#16）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`），或单文件
 * `npx tsx --test tests/unit/erp-cost-labor.test.ts`。
 *
 * 为什么测 `.ts` 不测 `.vue`：「金额会不会被当入参提交」「编辑只带真改过的字段」
 * 「工时超精度要不要拒」「分组合计怎么加」全在 `cost-labor-logic.ts`；`.vue` 只负责画。
 * 把 `v-if` 改成 `:disabled` 或把 PUT 整表回传，`vue-tsc` 抓不到。
 *
 * 断言口径与后端逐字对齐：
 * - 状态 / 来源值域：`ck_biz_cost_labor_status` / `ck_biz_cost_labor_src`
 * - 金额 = `round(hours × rate, 4)`，半数远离零
 * - 期间 / 日期：`normalize_period` + `ensure_date_in_period`（日期必须在期间内）
 * - 业务码：`66058`（金额不符）/ `66059`（三级费率全空）
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  COST_LABOR_PERMISSIONS,
  COST_LABOR_SOURCES,
  COST_LABOR_STATUSES,
  COST_RATE_MISSING_CODE,
  COST_LABOR_AMOUNT_CODE,
  type CostLaborRow,
} from '@/api/business/erp/cost-labor-kinds';
import {
  RATE_MISSING_HINT,
  canConfirmRow,
  canDeleteRow,
  canEditRow,
  emptyLaborForm,
  errorCount,
  expectedAmount,
  formFromRow,
  groupByObject,
  importSummaryText,
  isDateInPeriod,
  isDecimalWithinScale,
  isNonNegativeDecimal,
  isPeriodKey,
  isPositiveDecimal,
  isEmptyPayload,
  objectClosed,
  sourceLabel,
  statusLabel,
  statusTheme,
  sumDecimal,
  toCreatePayload,
  toUpdatePayload,
  validateLaborForm,
} from '@/views/business/erp/cost-labor/cost-labor-logic';

/** 造一行列表数据（只覆盖断言用到的字段） */
function rowOf(overrides: Partial<CostLaborRow> = {}): CostLaborRow {
  return {
    id: '11111111-1111-4111-8111-111111111111',
    period_code: '2026-11',
    work_date: '2026-11-05',
    cost_object_id: '22222222-2222-4222-8222-222222222222',
    cost_object_no: 'CB-20261101-000001',
    cost_object_status: 'OPEN',
    product_id: null,
    product_name: '钢轴',
    employee_id: null,
    employee_name: '张三',
    dept_id: '33333333-3333-4333-8333-333333333333',
    dept_name: '机加工车间',
    hours: '8.00',
    machine_hours: '0.00',
    rate: '50.000000',
    rate_scope: null,
    rate_scope_label: null,
    amount: '400.0000',
    source_type: 'MANUAL',
    status: 'DRAFT',
    remark: null,
    created_at: '2026-11-05T08:00:00+08:00',
    updated_at: '2026-11-05T08:00:00+08:00',
    ...overrides,
  };
}

// ==================== 词表 / 权限码 ====================

test('值域与权限码逐字对齐后端与 V0378 种子', () => {
  assert.deepEqual([...COST_LABOR_STATUSES], ['DRAFT', 'CONFIRMED']);
  assert.deepEqual([...COST_LABOR_SOURCES], ['MANUAL', 'IMPORT', 'MES']);
  assert.equal(COST_LABOR_PERMISSIONS.list, 'business:erp:cost-labor:list');
  assert.equal(COST_LABOR_PERMISSIONS.create, 'business:erp:cost-labor:create');
  assert.equal(COST_LABOR_PERMISSIONS.update, 'business:erp:cost-labor:update');
  assert.equal(COST_LABOR_PERMISSIONS.delete, 'business:erp:cost-labor:delete');
  assert.equal(COST_LABOR_PERMISSIONS.import, 'business:erp:cost-labor:import');
  // 「确认」复用 update（设计 §5.2 #14），不另立权限码
  assert.equal(Object.keys(COST_LABOR_PERMISSIONS).length, 5);
  assert.equal(COST_LABOR_AMOUNT_CODE, 66058);
  assert.equal(COST_RATE_MISSING_CODE, 66059);
});

test('状态 / 来源中文与主题', () => {
  assert.equal(statusLabel('DRAFT'), '草稿');
  assert.equal(statusLabel('CONFIRMED'), '已确认');
  assert.equal(statusLabel('WEIRD'), 'WEIRD', '未知值原样返回，不吞');
  assert.equal(statusTheme('DRAFT'), 'warning');
  assert.equal(statusTheme('CONFIRMED'), 'success');
  assert.equal(sourceLabel('IMPORT'), 'Excel 导入');
});

// ==================== 数值 / 期间 ====================

test('小数精度：尾零不算超精度，超精度必须拒', () => {
  assert.equal(isDecimalWithinScale('8.500', 2), true, '8.500 归一后 1 位');
  assert.equal(isDecimalWithinScale('8.55', 2), true);
  assert.equal(isDecimalWithinScale('8.555', 2), false, '列会把 8.555 舍成 8.56，工时错但金额对得上');
  assert.equal(isDecimalWithinScale('8', 2), true);
  assert.equal(isDecimalWithinScale('abc', 2), false);
  assert.equal(isDecimalWithinScale('-1', 2), true, '签名允许，正负由 isPositive 判');
  assert.equal(isPositiveDecimal('0', 2), false);
  assert.equal(isPositiveDecimal('0.01', 2), true);
  assert.equal(isNonNegativeDecimal('0', 2), true);
  assert.equal(isNonNegativeDecimal('-0.01', 2), false);
});

test('定点累加：字符串直接相加会拼接，必须走整数定点', () => {
  assert.equal(sumDecimal(['8.00', '2.50'], 2), '10.50');
  assert.equal(sumDecimal(['0.1', '0.2'], 2), '0.30', '浮点会在 0.30000000000000004 露馅');
  assert.equal(sumDecimal(['400.0000', '600.0000'], 4), '1000.0000');
  assert.equal(sumDecimal([null, undefined, ''], 2), '0.00');
  assert.equal(sumDecimal(['1.239'], 4), '1.2390', '不足位数补零');
});

test('预计金额 = round(工时 × 费率, 4)', () => {
  assert.equal(expectedAmount('8.00', '50'), '400.0000');
  assert.equal(expectedAmount('1', '33.333333'), '33.3333');
  assert.equal(expectedAmount('0.5', '0.5'), '0.2500');
  // 半数远离零：0.00005 × 1 元 → 0.0001（与后端 MidpointAwayFromZero 同口径）
  assert.equal(expectedAmount('1', '0.00005'), '0.0001');
});

test('期间 / 日期：工作日期必须落在期间内', () => {
  assert.equal(isPeriodKey('2026-11'), true);
  assert.equal(isPeriodKey('2026-13'), false);
  assert.equal(isPeriodKey('2026-1'), false);
  assert.equal(isDateInPeriod('2026-11-30', '2026-11'), true);
  assert.equal(isDateInPeriod('2026-12-01', '2026-11'), false);
  assert.equal(isDateInPeriod('2026-11-01', '2026-1'), false, '非法期间一律 false');
});

// ==================== 表单校验 ====================

test('表单校验：必填项逐个点名', () => {
  const empty = emptyLaborForm('2026-11-05');
  const errors = validateLaborForm(empty);
  assert.equal(errors.cost_object_id !== undefined, true);
  assert.equal(errors.employee_name !== undefined, true);
  assert.equal(errors.dept_id !== undefined, true);
  assert.equal(errors.hours !== undefined, true);
  assert.equal(errors.period_code, undefined, '期间由 today 推出，默认合法');
  assert.equal(errors.work_date, undefined);
  assert.equal(errorCount(errors) >= 4, true);
});

test('表单校验：日期不在期间内被拦（后端 ensure_date_in_period 的镜像）', () => {
  const form = {
    ...emptyLaborForm('2026-11-05'),
    cost_object_id: 'obj',
    employee_name: '张三',
    dept_id: 'dept',
    hours: '8',
    work_date: '2026-12-01',
  };
  const errors = validateLaborForm(form);
  assert.match(errors.work_date ?? '', /不在会计期间 2026-11 内/);
  assert.equal(errorCount(errors), 1, '其它字段都填对了');
});

test('表单校验：工时超精度 / 机器工时负数 / 费率格式', () => {
  const base = {
    ...emptyLaborForm('2026-11-05'),
    cost_object_id: 'obj',
    employee_name: '张三',
    dept_id: 'dept',
  };
  assert.match(validateLaborForm({ ...base, hours: '8.555' }).hours ?? '', /最多 2 位小数/);
  assert.match(validateLaborForm({ ...base, hours: '8', machine_hours: '-1' }).machine_hours ?? '', /≥ 0/);
  assert.match(validateLaborForm({ ...base, hours: '8', rate: 'abc' }).rate ?? '', /最多 6 位小数/);
  assert.equal(errorCount(validateLaborForm({ ...base, hours: '8', machine_hours: '', rate: '' })), 0);
});

// ==================== payload ====================

test('新建 payload：不带 amount（金额一律服务端算）', () => {
  const form = {
    ...emptyLaborForm('2026-11-05'),
    cost_object_id: 'obj-1',
    employee_name: ' 张三 ',
    dept_id: 'dept-1',
    hours: ' 8.00 ',
    machine_hours: '',
    rate: '',
    remark: ' ',
  };
  const payload = toCreatePayload(form);
  assert.equal(payload.period_code, '2026-11');
  assert.equal(payload.work_date, '2026-11-05');
  assert.equal(payload.cost_object_id, 'obj-1');
  assert.equal(payload.employee_name, '张三', 'trim');
  assert.equal(payload.hours, '8.00');
  assert.equal('amount' in payload, false, '绝不能提交 amount');
  assert.equal('machine_hours' in payload, false, '空的可选字段不发');
  assert.equal('rate' in payload, false, '留空 = 后端自动取数');
  assert.equal('remark' in payload, false);
});

test('编辑 payload：只带真改过的字段（部分更新 + 三态）', () => {
  const origin = rowOf();
  const form = formFromRow(origin);
  assert.equal(isEmptyPayload(toUpdatePayload(origin, form)), true, '原样回填不该产生 payload');

  const changed = { ...form, hours: '7.00', remark: '补录' };
  const payload = toUpdatePayload(origin, changed);
  assert.deepEqual(Object.keys(payload).sort(), ['hours', 'remark']);
  assert.equal(payload.hours, '7.00');
});

test('编辑 payload：换车间但费率留空 = 沿用快照（不自动重取）', () => {
  const origin = rowOf();
  const form = { ...formFromRow(origin), dept_id: 'dept-other' };
  const payload = toUpdatePayload(origin, form);
  assert.deepEqual(Object.keys(payload), ['dept_id']);
  assert.equal('rate' in payload, false, '自动重取会静默改写用户填过的费率');
});

// ==================== 行级按钮态 ====================

test('只有草稿能改 / 删 / 确认', () => {
  assert.equal(canEditRow({ status: 'DRAFT' }), true);
  assert.equal(canEditRow({ status: 'CONFIRMED' }), false);
  assert.equal(canDeleteRow({ status: 'DRAFT' }), true);
  assert.equal(canDeleteRow({ status: 'CONFIRMED' }), false);
  assert.equal(canConfirmRow({ status: 'DRAFT' }), true);
  assert.equal(canConfirmRow({ status: 'CONFIRMED' }), false, '已确认再点也是幂等成功，但按钮该灰');
  assert.equal(objectClosed({ cost_object_status: 'CLOSED' }), true);
  assert.equal(objectClosed({ cost_object_status: 'CANCELLED' }), true);
  assert.equal(objectClosed({ cost_object_status: 'OPEN' }), false);
});

// ==================== 分组 ====================

test('按成本对象分组：合计走定点，不串号', () => {
  const rows = [
    rowOf({ hours: '8.00', machine_hours: '2.00', amount: '400.0000', status: 'CONFIRMED' }),
    rowOf({
      id: 'other',
      hours: '2.50',
      machine_hours: '0.50',
      amount: '125.0000',
      employee_name: '李四',
      status: 'DRAFT',
    }),
    rowOf({
      id: 'third',
      cost_object_id: 'obj-2',
      cost_object_no: 'CB-20261101-000002',
      hours: '1.00',
      machine_hours: '0.00',
      amount: '50.0000',
    }),
  ];
  const groups = groupByObject(rows);
  assert.equal(groups.length, 2);
  const first = groups[0];
  assert.equal(first.cost_object_no, 'CB-20261101-000001');
  assert.equal(first.row_count, 2);
  assert.equal(first.people_count, 2);
  assert.equal(first.hours_total, '10.50');
  assert.equal(first.machine_hours_total, '2.50');
  assert.equal(first.amount_total, '525.0000');
  assert.equal(first.confirmed_count, 1);
  assert.equal(first.draft_count, 1);
  assert.equal(groups[1].hours_total, '1.00');
});

// ==================== 导入 ====================

test('导入回执：全有或全无的三种口径', () => {
  assert.match(importSummaryText({ success: 3, failed: 0 }), /导入成功：3 行已入库/);
  assert.match(importSummaryText({ success: 0, failed: 2 }), /整批拒绝：2 行有问题，一条都没入库/);
  assert.match(importSummaryText({ success: 0, failed: 0 }), /没有数据行/);
  // 文案里必须点明「半批入库会让当期人工成本少一块而没人发现」
  assert.match(importSummaryText({ success: 0, failed: 1 }), /人工成本少一块/);
});

test('66059 的提示必须说清"不按 0 计"', () => {
  assert.match(RATE_MISSING_HINT, /不会按 0 计/);
  assert.match(RATE_MISSING_HINT, /成本对象 > 车间 > 公司/);
});

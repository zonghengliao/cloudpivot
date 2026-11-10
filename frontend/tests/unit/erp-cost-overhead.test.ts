/**
 * 费用台账页纯逻辑单测（COST-B2-13 / ARCH-9 §6.1 #3 · §5.2 #17–#19）。
 *
 * 重点钉三件事（都在卡面验收里）：
 * ① 借方科目必须是 `4101.*` 且分段是数字（`66060` 的前置拦截）；
 * ② 金额 > 0 且 ≤ 4 位小数，**不静默舍入**（`66061`）；
 * ③ 池行 vs 直接行的判定与汇总（`cost_object_id IS NULL` 是唯一判据）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  COST_OVERHEAD_PERMISSIONS,
  DEFAULT_DEBIT_ACCOUNT,
  EXPENSE_ITEMS,
  OVERHEAD_ACCOUNT_CODE,
  OVERHEAD_AMOUNT_CODE,
  type CostOverheadRow,
} from '@/api/business/erp/cost-overhead-kinds';
import {
  emptyOverheadForm,
  errorCount,
  expenseItemLabel,
  expenseItemOptions,
  isManufacturingAccount,
  isPeriodKey,
  isPoolRow,
  pickCreditAccounts,
  pickManufacturingAccounts,
  summarizeRows,
  toCreatePayload,
  validateOverheadForm,
} from '@/views/business/erp/cost-overhead/cost-overhead-logic';

/** 造一行列表数据 */
function rowOf(overrides: Partial<CostOverheadRow> = {}): CostOverheadRow {
  return {
    id: '11111111-1111-4111-8111-111111111111',
    period_code: '2026-11',
    cost_object_id: null,
    cost_object_no: null,
    cost_object_status: null,
    dept_id: '22222222-2222-4222-8222-222222222222',
    dept_name: '一车间',
    expense_item: 'UTILITY',
    expense_item_label: '水电费',
    debit_account_code: '4101.02',
    debit_account_name: '制造费用-水电费',
    credit_account_code: '1002',
    credit_account_name: '银行存款',
    amount: '1000.0000',
    source_type: 'MANUAL',
    remark: null,
    created_at: '2026-11-05T08:00:00+08:00',
    updated_at: '2026-11-05T08:00:00+08:00',
    ...overrides,
  };
}

test('值域与权限码（设计 §5.2 只有 3 个码，没有 update）', () => {
  assert.deepEqual([...EXPENSE_ITEMS], [
    'DEPRECIATION',
    'UTILITY',
    'RENT',
    'REPAIR',
    'WORKSHOP_ADMIN',
    'CONSUMABLE',
    'OTHER',
  ]);
  assert.equal(Object.keys(COST_OVERHEAD_PERMISSIONS).length, 3);
  assert.equal('update' in COST_OVERHEAD_PERMISSIONS, false);
  assert.equal(DEFAULT_DEBIT_ACCOUNT, '4101.09');
  assert.equal(OVERHEAD_ACCOUNT_CODE, 66060);
  assert.equal(OVERHEAD_AMOUNT_CODE, 66061);
});

test('制造费用科目判定：与后端 is_manufacturing_account 同口径', () => {
  assert.equal(isManufacturingAccount('4101.01'), true);
  assert.equal(isManufacturingAccount('4101.09'), true);
  assert.equal(isManufacturingAccount(' 4101.1 '), true, 'trim 后判断');
  assert.equal(isManufacturingAccount('4101.'), false, '前缀之后再无内容 → 不是末级科目');
  assert.equal(isManufacturingAccount('4101'), false, '没有点，前缀不成立');
  assert.equal(isManufacturingAccount('4101.abc'), false);
  assert.equal(isManufacturingAccount('41011.01'), false);
  assert.equal(isManufacturingAccount('5602.01'), false, '销售费用不是制造费用');
});

test('科目下拉：借方只 4101.* 末级启用；贷方排除制造费用', () => {
  const accounts = [
    { account_code: '4101', account_name: '制造费用', is_leaf: false, status: 1 },
    { account_code: '4101.02', account_name: '制造费用-水电费', is_leaf: true, status: 1 },
    { account_code: '4101.03', account_name: '制造费用-修理费', is_leaf: true, status: 0 },
    { account_code: '1002', account_name: '银行存款', is_leaf: true, status: 1 },
    { account_code: '2241.01', account_name: '应付账款', is_leaf: true, status: 1 },
    { account_code: '1801', account_name: '累计折旧', is_leaf: false, status: 1 },
  ];
  const debit = pickManufacturingAccounts(accounts);
  assert.deepEqual(
    debit.map((item) => item.value),
    ['4101.02'],
    '非末级 / 停用 / 非 4101 都不进借方下拉',
  );
  assert.equal(debit[0].label, '4101.02 制造费用-水电费');
  const credit = pickCreditAccounts(accounts);
  assert.deepEqual(
    credit.map((item) => item.value),
    ['1002', '2241.01'],
    '贷方排除 4101.*；非末级 1801 也不进',
  );
});

test('费用项目名：字典实时名优先，落空回落内置文案', () => {
  assert.equal(expenseItemLabel('UTILITY', '水电费（改过名）'), '水电费（改过名）');
  assert.equal(expenseItemLabel('UTILITY', null), '水电费');
  assert.equal(expenseItemLabel('UTILITY', ''), '水电费');
  assert.equal(expenseItemLabel('UNKNOWN', null), 'UNKNOWN', '未知码原样返回，不编造');
  assert.equal(expenseItemOptions().length, 7);
});

test('池行判定：cost_object_id 为空就是池', () => {
  assert.equal(isPoolRow(rowOf({ cost_object_id: null })), true);
  assert.equal(isPoolRow(rowOf({ cost_object_id: '33333333-3333-4333-8333-333333333333' })), false);
});

test('表单校验：期间 / 车间 / 项目 / 借方 / 贷方 / 金额', () => {
  const empty = emptyOverheadForm('2026-11');
  assert.equal(isPeriodKey(empty.period_code), true);
  const errors = validateOverheadForm({ ...empty, amount: '' });
  assert.equal(errors.dept_id !== undefined, true);
  assert.equal(errors.credit_account_code !== undefined, true);
  assert.equal(errors.amount !== undefined, true);
  assert.equal(errors.debit_account_code, undefined, '缺省 4101.09 合法');
  assert.equal(errors.expense_item, undefined, '缺省折旧费');
});

test('表单校验：金额 0 / 负数 / 超精度都要拒（66061 的前置）', () => {
  const base = { ...emptyOverheadForm('2026-11'), dept_id: 'dept', credit_account_code: '1002' };
  assert.match(validateOverheadForm({ ...base, amount: '0' }).amount ?? '', /必须大于 0/);
  assert.match(validateOverheadForm({ ...base, amount: '-1' }).amount ?? '', /必须大于 0/);
  assert.match(validateOverheadForm({ ...base, amount: '8.55555' }).amount ?? '', /最多 4 位小数/);
  assert.match(validateOverheadForm({ ...base, amount: 'abc' }).amount ?? '', /最多 4 位小数/);
  assert.equal(errorCount(validateOverheadForm({ ...base, amount: '8.5555' })), 0, '4 位正好合法');
  assert.equal(errorCount(validateOverheadForm({ ...base, amount: '8.5000' })), 0, '尾零不算超精度');
});

test('表单校验：借方科目非 4101.* 直接拦（不白跑一次查库）', () => {
  const base = {
    ...emptyOverheadForm('2026-11'),
    dept_id: 'dept',
    credit_account_code: '1002',
    amount: '100',
  };
  assert.match(
    validateOverheadForm({ ...base, debit_account_code: '5602.01' }).debit_account_code ?? '',
    /必须是制造费用科目/,
  );
  assert.match(
    validateOverheadForm({ ...base, debit_account_code: '4101.' }).debit_account_code ?? '',
    /必须是制造费用科目/,
  );
});

test('新建 payload：缺省借方科目不发（让库默认 4101.09）', () => {
  const form = {
    ...emptyOverheadForm('2026-11'),
    dept_id: 'dept',
    credit_account_code: ' 1002 ',
    amount: '1000.0000',
    remark: ' ',
  };
  const payload = toCreatePayload(form);
  assert.equal(payload.period_code, '2026-11');
  assert.equal(payload.dept_id, 'dept');
  assert.equal(payload.credit_account_code, '1002');
  assert.equal('debit_account_code' in payload, false, '与 DDL 默认值一致时不必发');
  assert.equal('cost_object_id' in payload, false, '不选对象 = 车间池');
  assert.equal('remark' in payload, false);
});

test('新建 payload：显式借方科目与成本对象要发', () => {
  const form = {
    ...emptyOverheadForm('2026-11'),
    dept_id: 'dept',
    credit_account_code: '1002',
    amount: '100',
    debit_account_code: '4101.02',
    cost_object_id: 'obj-1',
  };
  const payload = toCreatePayload(form);
  assert.equal(payload.debit_account_code, '4101.02');
  assert.equal(payload.cost_object_id, 'obj-1');
});

test('本页汇总：池 / 直接分开合计（4 位定点）', () => {
  const rows = [
    rowOf({ amount: '1000.0000' }),
    rowOf({ id: 'b', amount: '300.0000' }),
    rowOf({
      id: 'c',
      cost_object_id: '33333333-3333-4333-8333-333333333333',
      cost_object_no: 'CB-20261101-000001',
      amount: '600.0000',
    }),
  ];
  const summary = summarizeRows(rows);
  assert.equal(summary.row_count, 3);
  assert.equal(summary.pool_count, 2);
  assert.equal(summary.pool_amount, '1300.0000');
  assert.equal(summary.direct_count, 1);
  assert.equal(summary.direct_amount, '600.0000');
  assert.equal(summary.total_amount, '1900.0000');
});

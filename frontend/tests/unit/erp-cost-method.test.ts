/**
 * 成本方法配置页纯逻辑单测（M5-01 · 品种法 / 分批法 / 标准成本）。
 *
 * 重点钉四件事（都在卡面验收与后端纪律里）：
 * ① **判定顺序**：对象级 > 全公司默认 > 代码兜底（分批法），停用行不参与；
 * ② **重叠预检**与后端 `find_overlap` 同判据（同范围 + 启用 + 区间相交，排除自己）；
 * ③ **已生效版本锁定**：`effective_from < 今天` 的版本只允许改停止日 / 状态 / 备注；
 * ④ **部分更新 payload**：只带真改过的字段；`effective_to` / `remark` 清空用空串表达。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  COST_METHOD_DEFAULT,
  COST_METHOD_IMPLEMENTED,
  COST_METHOD_LABELS,
  COST_METHOD_PERMISSIONS,
  COST_METHODS,
  COST_SCOPES,
  type CostMethodRow,
} from '@/api/business/erp/cost-method-kinds';
import {
  effectiveRangeText,
  emptyForm,
  findOverlapHint,
  formFromRow,
  isImplemented,
  isEmptyPayload,
  isLocked,
  isValid,
  methodLabel,
  methodNote,
  methodOptions,
  scopeDisplay,
  scopeLabel,
  sourceLabel,
  statusLabel,
  toCreatePayload,
  toUpdatePayload,
  validateForm,
} from '@/views/business/erp/cost-method/cost-method-logic';

/** 造一行列表数据 */
function rowOf(overrides: Partial<CostMethodRow> = {}): CostMethodRow {
  return {
    id: '11111111-1111-4111-8111-111111111111',
    method: 'JOB',
    method_label: '分批法',
    method_implemented: true,
    method_note: '按成本对象归集',
    scope: 'GLOBAL',
    scope_label: '全公司默认',
    scope_id: null,
    scope_object_no: null,
    scope_object_status: null,
    effective_from: '2026-01-01',
    effective_to: null,
    status: 1,
    status_label: '启用',
    remark: null,
    created_at: '2026-01-01T00:00:00+08:00',
    updated_at: '2026-01-01T00:00:00+08:00',
    ...overrides,
  };
}

/** 造一份表单（缺省按「指定成本对象 + 今天生效」） */
function formOf(overrides: Partial<ReturnType<typeof emptyForm>> = {}) {
  return {
    ...emptyForm('2026-11-18'),
    scope: 'COST_OBJECT',
    scope_id: '22222222-2222-4222-8222-222222222222',
    ...overrides,
  };
}

// ==================== 词表 ====================

test('方法词表与后端逐字一致，且一期只有分批法算得出来', () => {
  assert.deepEqual([...COST_METHODS], ['JOB', 'VARIETY', 'STANDARD']);
  assert.deepEqual([...COST_SCOPES], ['COST_OBJECT', 'GLOBAL']);
  assert.equal(COST_METHOD_LABELS.JOB, '分批法');
  assert.equal(COST_METHOD_LABELS.VARIETY, '品种法');
  assert.equal(COST_METHOD_LABELS.STANDARD, '标准成本');
  assert.equal(COST_METHOD_DEFAULT, 'JOB');
  assert.equal(COST_METHOD_IMPLEMENTED.JOB, true);
  assert.equal(COST_METHOD_IMPLEMENTED.VARIETY, false);
  assert.equal(COST_METHOD_IMPLEMENTED.STANDARD, false);
});

test('未知值原样返回、不吞（后端加了新方法时页面不会显示空白）', () => {
  assert.equal(methodLabel('ABC'), 'ABC');
  assert.equal(scopeLabel('SOMETHING'), 'SOMETHING');
  assert.equal(statusLabel(7), '7');
  assert.equal(sourceLabel('DEFAULT'), '代码兜底（未配置任何方法，默认分批法）');
  assert.equal(isImplemented('ABC'), false, '不确定的方法按「不能算」处理');
  assert.equal(methodNote('ABC'), '');
});

test('方法下拉把未实现的挂后缀，用户选之前就知道算不出来', () => {
  const byValue = new Map(methodOptions.map((option) => [option.value, option.label]));
  assert.equal(byValue.get('JOB'), '分批法');
  assert.ok(byValue.get('VARIETY')?.includes('一期未实现'));
  assert.ok(byValue.get('STANDARD')?.includes('一期未实现'));
});

test('权限码与 V0460 的 4 行菜单逐字一致', () => {
  assert.equal(COST_METHOD_PERMISSIONS.list, 'business:erp:cost-method:list');
  assert.equal(COST_METHOD_PERMISSIONS.create, 'business:erp:cost-method:create');
  assert.equal(COST_METHOD_PERMISSIONS.update, 'business:erp:cost-method:update');
  assert.equal(COST_METHOD_PERMISSIONS.delete, 'business:erp:cost-method:delete');
});

// ==================== 展示 ====================

test('生效期文案：空停止日显示「长期」而不是空串 / null', () => {
  assert.equal(effectiveRangeText(rowOf({ effective_from: '2026-01-01', effective_to: null })), '2026-01-01 ~ 长期');
  assert.equal(
    effectiveRangeText(rowOf({ effective_from: '2026-01-01', effective_to: '2026-06-30' })),
    '2026-01-01 ~ 2026-06-30',
  );
});

test('范围展示：全局显中文，对象级优先对象号，对象被删才回落 id', () => {
  assert.equal(scopeDisplay(rowOf({ scope: 'GLOBAL' })), '全公司默认');
  assert.equal(
    scopeDisplay(rowOf({ scope: 'COST_OBJECT', scope_object_no: 'CB-20261101-000001' })),
    'CB-20261101-000001',
  );
  const orphan = scopeDisplay(
    rowOf({ scope: 'COST_OBJECT', scope_object_no: null, scope_id: '33333333-3333-4333-8333-333333333333' }),
  );
  assert.ok(orphan.startsWith('33333333'), orphan);
});

test('口径锁定判据是「生效日 < 今天」（当天生效的当天还能改）', () => {
  assert.equal(isLocked(rowOf({ effective_from: '2026-11-17' }), '2026-11-18'), true);
  assert.equal(isLocked(rowOf({ effective_from: '2026-11-18' }), '2026-11-18'), false);
  assert.equal(isLocked(rowOf({ effective_from: '2026-11-19' }), '2026-11-18'), false);
});

// ==================== 表单校验 ====================

test('新建缺省：分批法 + 指定成本对象 + 今天生效 + 启用', () => {
  const form = emptyForm('2026-11-18');
  assert.equal(form.method, 'JOB');
  assert.equal(form.scope, 'COST_OBJECT');
  assert.equal(form.effective_from, '2026-11-18');
  assert.equal(form.effective_to, '');
  assert.equal(form.status, 1);
});

test('校验：对象级必须选对象、全局不许带对象、停止日不能早于生效日', () => {
  assert.equal(isValid(validateForm(formOf())), true);

  const noObject = validateForm(formOf({ scope_id: '' }));
  assert.ok(noObject.scope_id, '对象级没选对象必须报错');

  const globalWithObject = validateForm(formOf({ scope: 'GLOBAL' }));
  assert.ok(globalWithObject.scope_id, '全局带了对象必须报错');

  const badRange = validateForm(formOf({ effective_from: '2026-05-01', effective_to: '2026-04-30' }));
  assert.ok(badRange.effective_to, '停止日早于生效日必须报错');

  const badDate = validateForm(formOf({ effective_from: '2026/05/01' }));
  assert.ok(badDate.effective_from, '日期形状不对必须报错');
});

test('校验：合法区间（含同一天开始与结束）放行', () => {
  assert.equal(isValid(validateForm(formOf({ effective_from: '2026-05-01', effective_to: '2026-05-01' }))), true);
});

// ==================== 重叠预检 ====================

test('重叠预检：同范围 + 启用 + 相交才命中；不同范围 / 停用 / 排除自己都不算', () => {
  const objectId = '22222222-2222-4222-8222-222222222222';
  const existing = rowOf({
    id: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
    scope: 'COST_OBJECT',
    scope_id: objectId,
    effective_from: '2026-01-01',
    effective_to: '2026-06-30',
  });

  const overlapping = findOverlapHint([existing], formOf({ effective_from: '2026-06-01', effective_to: '' }));
  assert.equal(overlapping?.id, existing.id, '跨过旧版停止日必须命中');

  const adjacent = findOverlapHint([existing], formOf({ effective_from: '2026-07-01', effective_to: '' }));
  assert.equal(adjacent, null, '紧接旧版停止日次日应当放行');

  const otherObject = findOverlapHint(
    [existing],
    formOf({ scope_id: '44444444-4444-4444-8444-444444444444', effective_from: '2026-06-01' }),
  );
  assert.equal(otherObject, null, '不同对象互不影响');

  const globalScope = findOverlapHint([existing], formOf({ scope: 'GLOBAL', effective_from: '2026-06-01' }));
  assert.equal(globalScope, null, '全公司默认与对象级不同范围');

  const disabled = findOverlapHint(
    [{ ...existing, status: 0 }],
    formOf({ effective_from: '2026-06-01' }),
  );
  assert.equal(disabled, null, '停用行不参与判定，也不该挡住新版本');

  const self = findOverlapHint([existing], formOf({ effective_from: '2026-06-01' }), existing.id);
  assert.equal(self, null, '编辑自己不算重叠');
});

test('重叠预检：未填合法生效日时不猜（返回 null，交给后端最终判）', () => {
  assert.equal(findOverlapHint([rowOf()], formOf({ effective_from: '' })), null);
});

// ==================== payload ====================

test('新建 payload：全局清掉对象，空停止日 / 空备注传 null', () => {
  const payload = toCreatePayload(formOf({ scope: 'GLOBAL', effective_to: '', remark: '  ' }));
  assert.equal(payload.scope, 'GLOBAL');
  assert.equal(payload.scope_id, null);
  assert.equal(payload.effective_to, null);
  assert.equal(payload.remark, '');
});

test('修改 payload：只带真改过的字段；清空停止日 / 备注用空串', () => {
  const row = rowOf({
    id: 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb',
    scope: 'COST_OBJECT',
    scope_id: '22222222-2222-4222-8222-222222222222',
    effective_from: '2026-12-01',
    effective_to: '2026-12-31',
    remark: '旧备注',
    status: 1,
  });

  const noop = toUpdatePayload(formFromRow(row), row);
  assert.equal(isEmptyPayload(noop), true, '什么都没改就不该发请求');

  const closed = toUpdatePayload(
    { ...formFromRow(row), effective_to: '', remark: '' },
    row,
  );
  assert.equal(closed.effective_to, '', '空串 = 清空（后端把空串解释成清空）');
  assert.equal(closed.remark, '', '空串 = 清空备注');
  assert.equal(closed.method, undefined, '没改的字段不能带');

  const switched = toUpdatePayload({ ...formFromRow(row), method: 'VARIETY' }, row);
  assert.equal(switched.method, 'VARIETY');
  assert.deepEqual(Object.keys(switched), ['method']);
});

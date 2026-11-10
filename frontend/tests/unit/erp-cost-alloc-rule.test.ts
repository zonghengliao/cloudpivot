/**
 * 分摊规则页纯逻辑单测（COST-B2-13 / ARCH-9 §6.2.3 · §4.4 · §5.2 #20–#24）。
 *
 * 重点钉四件事（都在卡面验收里）：
 * ① **试算守恒**：Σ分摊额 == 池（后端固定 4 位小数，字面比较会抖 → `decimalEquals`）；
 * ② **版本化**：`effective_to` 传空串 = 清空（长期有效）、不传 = 不改；
 * ③ **规则编码归一**：小写自动转大写，形状只允许大写字母/数字/_-.；
 * ④ **尾差只有一行**（最大余数法），页面要把它标出来而不是当异常。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  ALLOC_BASIS_LABELS,
  ALLOC_BASES,
  ALLOC_NO_OBJECT_CODE,
  ALLOC_BASIS_ZERO_CODE,
  COST_ALLOC_RULE_PERMISSIONS,
  PRIORITY_DEFAULT,
  type CostAllocPreviewGroup,
  type CostAllocPreviewRes,
  type CostAllocRuleRow,
} from '@/api/business/erp/cost-alloc-rule-kinds';
import {
  RULE_LOCKED_HINT,
  basisHint,
  basisLabel,
  basisOptions,
  basisSharePercent,
  effectiveRangeText,
  emptyRuleForm,
  errorCount,
  formFromRow,
  groupConserved,
  isEmptyPayload,
  isPeriodKey,
  isRuleCodeShape,
  normalizeRuleCode,
  previewConserved,
  previewEmpty,
  previewSummaryText,
  roundingRows,
  statusLabel,
  toCreatePayload,
  toUpdatePayload,
  validateRuleForm,
} from '@/views/business/erp/cost-alloc-rule/cost-alloc-rule-logic';

/** 造一行规则 */
function ruleOf(overrides: Partial<CostAllocRuleRow> = {}): CostAllocRuleRow {
  return {
    id: '11111111-1111-4111-8111-111111111111',
    rule_code: 'DEPT1_LABOR',
    rule_name: '一车间制造费用按工时分摊',
    biz_type: 'OVERHEAD',
    source_dept_id: '22222222-2222-4222-8222-222222222222',
    source_dept_name: '一车间',
    account_code: '4101.01',
    account_name: '制造费用-折旧费',
    basis: 'LABOR_HOURS',
    basis_label: '按人工工时分摊',
    priority: 10,
    effective_from: '2026-11-01',
    effective_to: null,
    status: 1,
    status_label: '启用',
    remark: null,
    created_at: '2026-11-01T08:00:00+08:00',
    updated_at: '2026-11-01T08:00:00+08:00',
    ...overrides,
  };
}

/** 造一条试算组（1000 元池，1:1:1 基数，尾差 0.0001 只落一行） */
function groupOf(overrides: Partial<CostAllocPreviewGroup> = {}): CostAllocPreviewGroup {
  return {
    rule_id: 'r-1',
    rule_code: 'DEPT1_LABOR',
    rule_name: '一车间制造费用按工时分摊',
    source_dept_id: '22222222-2222-4222-8222-222222222222',
    source_dept_name: '一车间',
    account_code: '4101.01',
    account_name: '制造费用-折旧费',
    basis: 'LABOR_HOURS',
    basis_label: '按人工工时分摊',
    pool_amount: '1000.0000',
    basis_total: '3.0000',
    alloc_total: '1000.0000',
    rounding_total: '0.0001',
    rows: [
      {
        cost_object_id: 'o-1',
        cost_object_no: 'CB-20261101-000001',
        cost_object_status: 'OPEN',
        basis_value: '1.0000',
        alloc_amount: '333.3334',
        rounding_adj: '0.0001',
      },
      {
        cost_object_id: 'o-2',
        cost_object_no: 'CB-20261101-000002',
        cost_object_status: 'OPEN',
        basis_value: '1.0000',
        alloc_amount: '333.3333',
        rounding_adj: '0.0000',
      },
      {
        cost_object_id: 'o-3',
        cost_object_no: 'CB-20261101-000003',
        cost_object_status: 'OPEN',
        basis_value: '1.0000',
        alloc_amount: '333.3333',
        rounding_adj: '0.0000',
      },
    ],
    ...overrides,
  };
}

function previewOf(overrides: Partial<CostAllocPreviewRes> = {}): CostAllocPreviewRes {
  return {
    period_code: '2026-11',
    groups: [groupOf()],
    total_pool_amount: '1000.0000',
    total_alloc_amount: '1000.0000',
    unallocated: [],
    ...overrides,
  };
}

test('值域 / 权限码 / 业务码逐字对齐后端', () => {
  assert.deepEqual([...ALLOC_BASES], ['LABOR_HOURS', 'MACHINE_HOURS', 'OUTPUT_QTY', 'MATERIAL_COST']);
  assert.equal(ALLOC_BASIS_LABELS.LABOR_HOURS, '按人工工时分摊');
  assert.equal(Object.keys(COST_ALLOC_RULE_PERMISSIONS).length, 5);
  assert.equal(COST_ALLOC_RULE_PERMISSIONS.preview, 'business:erp:cost-alloc-rule:preview');
  assert.equal(ALLOC_NO_OBJECT_CODE, 66062);
  assert.equal(ALLOC_BASIS_ZERO_CODE, 66063);
  assert.equal(PRIORITY_DEFAULT, 10);
  assert.equal(statusLabel(1), '启用');
  assert.equal(statusLabel(0), '停用');
  assert.equal(basisLabel('MACHINE_HOURS'), '按机器工时分摊');
  assert.equal(basisLabel('WEIRD'), 'WEIRD', '未知值原样返回');
});

test('分配标准下拉带一句解释（设计 §6.2.3 要求）', () => {
  const options = basisOptions();
  assert.equal(options.length, 4);
  assert.match(options[0].label, /按人工工时分摊 —— /);
  assert.equal(options[0].value, 'LABOR_HOURS');
  assert.match(basisHint('OUTPUT_QTY'), /完工数量/);
});

test('生效期文案：有限期 / 长期', () => {
  assert.equal(effectiveRangeText(ruleOf()), '2026-11-01 起（长期）');
  assert.equal(effectiveRangeText(ruleOf({ effective_to: '2026-12-31' })), '2026-11-01 ~ 2026-12-31');
});

test('规则编码：trim + 大写归一 + 形状校验', () => {
  assert.equal(normalizeRuleCode('  dept1_labor  '), 'DEPT1_LABOR');
  assert.equal(isRuleCodeShape('DEPT1_LABOR'), true);
  assert.equal(isRuleCodeShape('A.B-1_2'), true);
  assert.equal(isRuleCodeShape('DEPT 1'), false, '空格不允许');
  assert.equal(isRuleCodeShape('部门1'), false, '中文不允许');
});

test('表单校验：必填 + 编码形状 + 优先级区间 + 生效期先后', () => {
  const empty = emptyRuleForm('2026-11-05', 'dept-1');
  const errors = validateRuleForm(empty);
  assert.equal(errors.dept_id, undefined, '车间可预填');
  assert.equal(errors.rule_code !== undefined, true);
  assert.equal(errors.rule_name !== undefined, true);
  assert.equal(errors.account_code !== undefined, true);
  assert.equal(errors.effective_from, undefined, '默认当月 1 日');
  assert.equal(empty.effective_from, '2026-11-01');

  const filled = {
    ...empty,
    rule_code: 'r1',
    rule_name: '规则一',
    account_code: '4101.01',
  };
  assert.equal(errorCount(validateRuleForm(filled)), 0, '小写编码会被归一，不算错');

  assert.match(validateRuleForm({ ...filled, priority: '0' }).priority ?? '', /1~999/);
  assert.match(validateRuleForm({ ...filled, priority: '1000' }).priority ?? '', /1~999/);
  assert.match(validateRuleForm({ ...filled, priority: '1.5' }).priority ?? '', /1~999/);

  const badRange = { ...filled, effective_to: '2026-10-31' };
  assert.match(validateRuleForm(badRange).effective_to ?? '', /不能早于开始日/);

  const longCode = { ...filled, rule_code: 'X'.repeat(33) };
  assert.match(validateRuleForm(longCode).rule_code ?? '', /最长 32 个字符/);
});

test('新建 payload：编码归一、优先级转数、空终止日不发', () => {
  const form = {
    ...emptyRuleForm('2026-11-05', 'dept-1'),
    rule_code: ' dept1_labor ',
    rule_name: ' 一车间制造费用按工时分摊 ',
    account_code: ' 4101.01 ',
  };
  const payload = toCreatePayload(form);
  assert.equal(payload.rule_code, 'DEPT1_LABOR');
  assert.equal(payload.rule_name, '一车间制造费用按工时分摊');
  assert.equal(payload.account_code, '4101.01');
  assert.equal(payload.priority, 10);
  assert.equal('effective_to' in payload, false, '不填 = 长期有效');
  assert.equal('status' in payload, false, '启用是缺省，不必发');
});

test('编辑 payload：原样回填不产生请求；空终止日 = 清空（三态）', () => {
  const origin = ruleOf();
  assert.equal(isEmptyPayload(toUpdatePayload(origin, formFromRow(origin))), true);

  // 有限期 → 改成长期：必须显式发空串
  const limited = ruleOf({ effective_to: '2026-12-31' });
  const cleared = toUpdatePayload(limited, { ...formFromRow(limited), effective_to: '' });
  assert.deepEqual(cleared, { effective_to: '' });

  // 只改名称 + 状态（已参与分摊的版本唯一放行的字段）
  const renamed = toUpdatePayload(origin, { ...formFromRow(origin), rule_name: '改名', status: 0 });
  assert.deepEqual(Object.keys(renamed).sort(), ['rule_name', 'status']);
});

test('试算守恒：Σ分摊额 == 池（不同位数也算等）', () => {
  assert.equal(groupConserved(groupOf()), true);
  assert.equal(groupConserved(groupOf({ alloc_total: '999.9999' })), false);
  assert.equal(
    groupConserved(groupOf({ pool_amount: '1000.00', alloc_total: '1000.0000' })),
    true,
    '后端固定 4 位，字面比较会抖',
  );
  assert.equal(previewConserved(previewOf()), true);
  assert.equal(previewConserved(previewOf({ total_alloc_amount: '1000.0001' })), false);
});

test('尾差行：最大余数法只有一行非 0', () => {
  const rows = roundingRows(groupOf());
  assert.equal(rows.length, 1);
  assert.equal(rows[0].cost_object_no, 'CB-20261101-000001');
  assert.equal(rows[0].rounding_adj, '0.0001');
  assert.equal(roundingRows(groupOf({ rows: [] })).length, 0);
});

test('占比展示：基数为 0 不除零', () => {
  const group = groupOf();
  assert.equal(basisSharePercent({ basis_value: '1.0000' }, group), '33.33%');
  assert.equal(basisSharePercent({ basis_value: '0.0000' }, { ...group, basis_total: '0.0000' }), '0.00%');
});

test('试算空态与摘要文案', () => {
  assert.equal(previewEmpty(null), true);
  assert.equal(previewEmpty(previewOf()), false);
  assert.equal(previewEmpty(previewOf({ groups: [] })), true);
  assert.equal(
    previewEmpty(previewOf({ groups: [], unallocated: [{ source_dept_id: null, source_dept_name: null, account_code: null, account_name: null, pool_amount: '10.0000', reason: '没有生效规则' }] })),
    false,
    '有诊断就不算空',
  );
  assert.match(previewSummaryText(previewOf()), /池 1000.0000 \/ 摊 1000.0000 · 守恒 ✓/);
  assert.match(
    previewSummaryText(previewOf({ total_alloc_amount: '1.0000' })),
    /不守恒 ✗/,
    '不守恒必须显眼，别当成正常',
  );
});

test('期间判定与"已参与分摊"的引导文案', () => {
  assert.equal(isPeriodKey('2026-11'), true);
  assert.equal(isPeriodKey('2026-11-01'), false);
  assert.match(RULE_LOCKED_HINT, /新建一条生效期更晚的版本/);
  assert.match(RULE_LOCKED_HINT, /不溯及历史/);
});

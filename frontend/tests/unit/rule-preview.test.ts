/**
 * `rule-preview.ts` 单测 —— 规则预览（人话翻译）的六种形态。
 *
 * 运行：`pnpm test:unit`。
 *
 * 卡面要求逐条钉死：单条件 / 双条件 AND / OR 混合（括号）/ 嵌套括号 / 空树 / 非法节点。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  EMPTY_TEXT,
  OP_WORDS,
  cmpSentence,
  conditionSentence,
  emptyFieldCount,
  literalText,
  targetPhrase,
  thenPhrase,
  translateRule,
} from '../../src/views/business/flow/components/rule-preview.ts';
import { createTarget, type ThenTarget } from '../../src/views/business/flow/components/then-target.ts';
import { validateRule } from '../../src/views/business/flow/components/rule-validation.ts';

/** 比较节点 */
const cmp = (field: string, op = 'gte', value: unknown = 10000): Record<string, unknown> => ({
  op: 'cmp',
  field,
  cmp: op,
  value,
});

/** 分组 */
const group = (op: 'and' | 'or', args: unknown[]): Record<string, unknown> => ({ op, args });

/** 字段中文名（模拟编辑器接后端字段字典） */
const LABELS: Record<string, string> = {
  'form.amount': '金额',
  'form.dept': '部门',
  'task.status': '任务状态',
};

/** 一个角色审批目标 */
const roleTarget = (): ThenTarget => ({ ...createTarget('ROLE'), roleCodes: ['FINANCE_DIRECTOR'] });

test('预览 · 单条件 + then：当 金额 ≥ 10000 时 → 由 角色 FINANCE_DIRECTOR 审批', () => {
  const preview = translateRule({
    condition: cmp('form.amount'),
    targets: [roleTarget()],
    fieldLabels: LABELS,
  });
  assert.equal(preview.empty, false);
  assert.deepEqual(preview.lines, ['当 金额 ≥ 10000 时', '→ 由 角色 FINANCE_DIRECTOR 审批']);
  assert.equal(preview.sentence, '当 金额 ≥ 10000 时 → 由 角色 FINANCE_DIRECTOR 审批');
  assert.deepEqual(preview.warnings, []);
});

test('预览 · 双条件 AND：当 金额 ≥ 10000 且 部门 是 财务 时', () => {
  const preview = translateRule({
    condition: group('and', [cmp('form.amount'), cmp('form.dept', 'eq', '财务')]),
    targets: [roleTarget()],
    fieldLabels: LABELS,
  });
  assert.equal(preview.lines[0], '当 金额 ≥ 10000 且 部门 是 财务 时');
  assert.equal(preview.report.counts.rules, 2);
});

test('预览 · OR 混合：不同算子的子分组必须加括号', () => {
  const preview = translateRule({
    condition: group('or', [
      group('and', [cmp('form.amount'), cmp('form.dept', 'eq', '财务')]),
      cmp('task.status', 'eq', 'DONE'),
    ]),
    targets: [roleTarget()],
    fieldLabels: LABELS,
  });
  assert.equal(preview.lines[0], '当 (金额 ≥ 10000 且 部门 是 财务) 或 任务状态 是 DONE 时');
});

test('预览 · 同算子嵌套摊平（不丢语义、少一层括号）', () => {
  const tree = group('and', [cmp('form.amount'), group('and', [cmp('form.dept', 'eq', '财务'), cmp('task.status', 'eq', 'DONE')])]);
  assert.equal(
    conditionSentence(tree as never, LABELS),
    '金额 ≥ 10000 且 部门 是 财务 且 任务状态 是 DONE',
  );
});

test('预览 · not 单独加括号：当 非(部门 是 财务) 时', () => {
  const preview = translateRule({
    condition: { op: 'not', args: [cmp('form.dept', 'eq', '财务')] },
    targets: [roleTarget()],
    fieldLabels: LABELS,
  });
  assert.equal(preview.lines[0], '当 非(部门 是 财务) 时');
});

test('预览 · 空规则树 → 「尚未配置规则」', () => {
  const preview = translateRule({ condition: null, targets: [roleTarget()] });
  assert.equal(preview.empty, true);
  assert.deepEqual(preview.lines, [EMPTY_TEXT]);
  assert.equal(preview.sentence, '尚未配置规则');
  assert.equal(preview.report.ok, true);
});

test('预览 · 非法节点（空字段）→ ⚠ 第 1 条规则有空字段', () => {
  const preview = translateRule({
    condition: group('and', [cmp(''), cmp('form.amount')]),
    targets: [roleTarget()],
    fieldLabels: LABELS,
  });
  assert.deepEqual(preview.warnings, ['⚠ 第 1 条规则有空字段']);
  assert.equal(preview.report.ok, false);
  // 句子里也能看出占位，而不是静默省掉一条
  assert.equal(preview.lines[0], '当 （未选字段） ≥ 10000 且 金额 ≥ 10000 时');
  assert.equal(emptyFieldCount(group('and', [cmp(''), cmp('form.amount')])), 0 + 1);
});

test('预览 · 非白名单字段 → 横幅提示带后端原句（不是「空字段」）', () => {
  const preview = translateRule({ condition: cmp('user.nickname'), targets: [roleTarget()] });
  assert.equal(preview.warnings.length, 1);
  assert.ok((preview.warnings[0] as string).startsWith('⚠ 第 1 条规则：字段 user.nickname 不在白名单内'));
});

test('预览 · then 目标为空 → 句子占位 + 一条 65016 提醒', () => {
  const preview = translateRule({ condition: cmp('form.amount'), targets: [], fieldLabels: LABELS });
  assert.equal(preview.lines[1], '→ 由 （未配置审批人） 审批');
  assert.equal(preview.warnings.length, 1);
  assert.ok((preview.warnings[0] as string).includes('的审批人不能为空（至少 1 个）'));
  assert.equal(preview.report.codes[0], 65016);
});

test('目标读法：10 种来源都能读成人话，保留词标「不支持」', () => {
  assert.equal(targetPhrase(createTarget('INITIATOR')), '发起人');
  assert.equal(targetPhrase({ ...createTarget('INITIATOR_SUPERIOR'), levels: 2 }), '发起人上级（上溯 2 级）');
  assert.equal(
    targetPhrase({ ...createTarget('DEPT_LEADER'), deptIds: [] }),
    '发起人部门负责人',
  );
  assert.equal(
    targetPhrase({ ...createTarget('USER'), userIds: ['a', 'b'] }),
    '指定用户（2 人）',
  );
  assert.equal(
    targetPhrase({ ...createTarget('FORM_VAR'), formVarKey: 'owner_id' }),
    '表单字段「owner_id」指向的人',
  );
  assert.equal(targetPhrase({ ...createTarget('USER'), type: 'PREV_APPROVER' }), '（不支持的来源 PREV_APPROVER）');
  assert.equal(thenPhrase([]), '（未配置审批人）');
});

test('算术子与字面量读法：数组用「、」、空值有明确占位', () => {
  assert.equal(literalText(['A', 'B']), 'A、B');
  assert.equal(literalText([]), '（空列表）');
  assert.equal(literalText(''), '（空）');
  assert.equal(literalText(0), '0');
  assert.equal(OP_WORDS.is_null, '为空');
  assert.equal(cmpSentence({ op: 'cmp', field: 'task.status', cmp: 'is_null', value: null }, LABELS), '任务状态 为空');
});

test('预览 · 面板直接可用的计数（8/8 层、23/64、2/32 条）', () => {
  const condition = group('and', [cmp('form.amount'), cmp('form.dept', 'eq', '财务')]);
  const preview = translateRule({ condition, targets: [roleTarget()], fieldLabels: LABELS });
  assert.deepEqual(preview.report.counts, { depth: 2, nodes: 3, rules: 2, targets: 1 });
  const report = validateRule({ condition, targets: [roleTarget()] });
  assert.deepEqual(report.limits, { depth: 8, nodes: 64, rules: 32, targets: 5 });
});

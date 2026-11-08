/**
 * P3-08：子流程节点配置的纯函数单测。
 *
 * 钉住三件容易错的事：
 * 1. `config.sub` 的默认值与后端 `parse_config` 一致（`PIN_CURRENT` + `ABORT`）；
 * 2. 改 `config.sub` 不覆盖别的 config 分区（`task` / `notify` 一起存在时不丢）；
 * 3. 两套映射的方向：`param_mapping` 键是**子流程键**、`result_mapping` 键是**父表单路径**，
 *    空键 / 空值的行在收敛时被丢掉（后端把空键当 65045）。
 */
import assert from 'node:assert/strict';
import test from 'node:test';

import {
  DEFAULT_SUB_CONFIG,
  mappingRowsOf,
  rowsToMapping,
  subConfigOf,
  subConfigPatch,
} from '../../src/views/business/flow/components/subprocess-node-config';
import { makeNode } from '../../src/views/business/flow/graph';

test('子流程节点拖进来就带默认 config.sub', () => {
  const node = makeNode('SUBPROCESS', { x: 100, y: 100 }, [], '调用预算调整');
  const sub = (node.config as { sub?: Record<string, unknown> }).sub;
  assert.ok(sub, '必须有 config.sub（否则后端 R27 报 65045）');
  assert.equal(sub?.version_policy, 'PIN_CURRENT');
  assert.equal(sub?.fail_action, 'ABORT');
  assert.equal(node.subprocess_def_id ?? null, null, '定义必须由用户选');
});

test('subConfigOf 对坏形状不炸（返回空对象，面板用默认值兜底）', () => {
  assert.deepEqual(subConfigOf({ config: { sub: 'oops' } }), {});
  assert.deepEqual(subConfigOf({ config: null }), {});
  assert.deepEqual(subConfigOf(null), {});
});

test('subConfigPatch 只动 sub，不覆盖 task / notify 分区', () => {
  const node = { config: { task: { title_template: 'x' }, notify: { when: 'ON_ENTER' }, sub: { fail_action: 'ABORT' as const } } };
  const patch = subConfigPatch(node, { fail_action: 'CONTINUE' });
  const config = patch.config as Record<string, unknown>;
  assert.deepEqual(config.task, { title_template: 'x' });
  assert.deepEqual(config.notify, { when: 'ON_ENTER' });
  assert.equal((config.sub as { fail_action?: string }).fail_action, 'CONTINUE');
});

test('默认配置与后端缺省值逐字一致', () => {
  assert.equal(DEFAULT_SUB_CONFIG.version_policy, 'PIN_CURRENT');
  assert.equal(DEFAULT_SUB_CONFIG.fail_action, 'ABORT');
  assert.equal(DEFAULT_SUB_CONFIG.spawn_task, false);
});

test('映射行列转换：丢掉空键 / 空值行', () => {
  const rows = [
    { key: 'amount', value: 'expense.amount' },
    { key: '  ', value: 'x' },
    { key: 'dept', value: '   ' },
  ];
  assert.deepEqual(rowsToMapping(rows), { amount: 'expense.amount' });
  assert.deepEqual(mappingRowsOf({ a: 'b.c', d: 'e' }), [
    { key: 'a', value: 'b.c' },
    { key: 'd', value: 'e' },
  ]);
  assert.deepEqual(mappingRowsOf(undefined), []);
});

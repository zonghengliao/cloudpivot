/**
 * 「`on_complete=APPROVE` → 主按钮下方显示「完成后需审批」」的回归单测（P-FLOW-1C 补漏）。
 *
 * # 这条回归是怎么来的
 *
 * `GET /business/task/{id}/flow` 在 P-FLOW-1A 里没下发 `on_complete`，前端的完成方式
 * 只能按 `need_review` 推断（`true` → `VERIFY`，否则 `DIRECT`）。于是节点配了
 * `on_complete=APPROVE` 的流程任务被推断成 `DIRECT`，`flowMainNote()` 返回 `null`，
 * **副文案「完成后需审批」永远不显示** —— 用户点完「完成」看到节点还在「处理中」会以为没生效。
 *
 * 后端 `138d39a` 补齐三个字段后，前端 `TaskFlowDetail` 把 `on_complete` / `on_reject` /
 * `on_cancel` 声明成**必填的 `string | null`**。本文件钉住两件事：
 *
 * 1. 接口真下发 `APPROVE` 时，推导出来的完成方式**就是** `APPROVE`（不能被推断覆盖）；
 * 2. 只有真 `null`（节点没配该项）才走 `need_review` 推断，此时副文案为空。
 *
 * 数据取自父卡 `t_00f3a5b8` 用 curl 打真接口拿到的样例（`START → TASK_NODE(催货) → END`），
 * 不是编的。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  applyFlowMainButton,
  flowCompletionFrom,
  flowMainLabel,
  flowMainNote,
  type FlowCompletion,
} from '../../src/views/business/task/components/flow-actions.ts';
import type { TaskFlowDetail } from '../../src/api/business/task-api.ts';

/**
 * 真接口样例（`on_complete=APPROVE`，`on_cancel` 没配 → `null`）。
 *
 * 注意 `on_cancel: null`：三个字段**恒出现**，没配的给 `null` 而不是省略 ——
 * 这正是把类型写成必填 `string | null` 而不是可选的依据。
 */
const APPROVE_SAMPLE: TaskFlowDetail = {
  instance_id: '2b1f0f0e-0000-4000-8000-000000000001',
  instance_no: 'FLW-20261101-3F9E2215',
  flow_code: 'FIX1C_APPROVE',
  flow_name: 'FIX1C approve 流程',
  biz_type: 'TASK',
  biz_no: '',
  instance_status: 'PENDING',
  initiator_id: '01a18171-e94a-7537-b822-d75e6566845c',
  initiator_name: 'Henry.Liao',
  started_at: '2026-11-01T05:00:00Z',
  ended_at: null,
  node_key: 'n_task',
  node_name: '催货',
  round: 1,
  node_status: 'ACTIVE',
  on_complete: 'APPROVE',
  on_reject: 'TERMINATE',
  on_cancel: null,
  approval: null,
};

/** `DOING` 态的两个完成类按钮（`applyFlowMainButton` 要收敛的对象） */
function doingActions(): Array<{ key: string; label: string }> {
  return [
    { key: 'direct-done', label: '直接完成' },
    { key: 'submit-review', label: '提交验收' },
    { key: 'cancel', label: '取消' },
  ];
}

test('真接口样例 APPROVE → 完成方式是 APPROVE，副文案「完成后需审批」显示', () => {
  const completion = flowCompletionFrom(APPROVE_SAMPLE, false);
  // 关键断言：后端给了 APPROVE 就不能被 `need_review` 推断成 DIRECT（旧回归）
  assert.equal(completion?.on_complete, 'APPROVE');
  assert.equal(flowMainNote(completion), '完成后需审批');
  // 主按钮仍是「完成」——APPROVE 不改按钮文案，只加副文案
  assert.equal(flowMainLabel(completion), '完成');
});

test('APPROVE 走到按钮组：只剩一个主按钮「完成」+ 取消，副文案仍可见', () => {
  const completion = flowCompletionFrom(APPROVE_SAMPLE, false);
  const list = applyFlowMainButton(doingActions(), completion);
  assert.deepEqual(
    list.map((item) => item.key),
    ['direct-done', 'cancel'],
  );
  assert.equal(list[0]?.label, '完成');
  assert.equal(flowMainNote(completion), '完成后需审批');
});

test('on_complete=null（没配该项）+ 无需验收 → 推断 DIRECT，无副文案', () => {
  const completion = flowCompletionFrom({ ...APPROVE_SAMPLE, on_complete: null }, false);
  assert.equal(completion?.on_complete, 'DIRECT');
  assert.equal(flowMainNote(completion), null);
});

test('on_complete=null（没配该项）+ 需验收 → 推断 VERIFY，主按钮「提交验收」，无副文案', () => {
  const completion = flowCompletionFrom({ ...APPROVE_SAMPLE, on_complete: null }, true);
  assert.equal(completion?.on_complete, 'VERIFY');
  assert.equal(flowMainLabel(completion), '提交验收');
  assert.equal(flowMainNote(completion), null);
});

test('手工任务（flow=null）→ 完成上下文为 null，且不改变原有按钮', () => {
  const completion: FlowCompletion | null = flowCompletionFrom(null, false);
  assert.equal(completion, null);
  assert.equal(flowMainNote(completion), null);
  const actions = doingActions();
  assert.deepEqual(applyFlowMainButton(actions, completion), actions);
});

test('三个走向字段是必填：样例里 on_cancel=null 也在，类型上不能省', () => {
  // 运行时验证「字段在对象上」（`null` 与「缺字段」语义不同，后端承诺的是前者）
  assert.ok('on_complete' in APPROVE_SAMPLE);
  assert.ok('on_reject' in APPROVE_SAMPLE);
  assert.ok('on_cancel' in APPROVE_SAMPLE);
  assert.equal(APPROVE_SAMPLE.on_cancel, null);
  assert.equal(APPROVE_SAMPLE.on_reject, 'TERMINATE');
});

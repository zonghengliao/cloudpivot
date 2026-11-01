/**
 * 「任务：已完成 ✓ 审批中（王五）」的渲染判据单测（P-FLOW-1B 收尾 · 设计 §4.5 Q4 第 3 条）。
 *
 * # 这条为什么必须单测
 *
 * 判据里「什么时候**不**显示」比「什么时候显示」更容易错：
 * `approval=null`（DIRECT 节点）、`APPROVED`（早就批完了）、`pending_approvers=[]`（没人可等）
 * 三种情况都长得像「任务已完成、节点还在跑」—— 只要有一条走漏，界面就会对用户说
 * 「审批中」，而实际没有任何人在审。这正是 1B 之前「前端骗用户」的老路，必须先钉死。
 *
 * 样例取自父卡 `t_b3f8c3b0` 的真接口回执（`GET /business/task/{id}/flow`，commit `e14f4e6`），
 * 不是编的形状：`{instance_id, instance_no, status, pending_approvers}`，字段恒出现。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  FLOW_APPROVAL_INLINE_MAX,
  flowApprovalSuffix,
  type FlowApprovalState,
} from '../../src/views/business/task/components/flow-actions.ts';
import type { TaskFlowDetail } from '../../src/api/business/task-api.ts';

/** 真接口回执（`on_complete=APPROVE`：任务 DONE、节点仍 ACTIVE、审批单 PENDING、待办人王五） */
const PENDING_FLOW: TaskFlowDetail = {
  instance_id: '01a195b1-70a1-77b2-8765-0cdb66e6d59c',
  instance_no: 'FLW-20261101-3D798C31',
  flow_code: 'PFLOW1B_APPROVE_11',
  flow_name: 'P-FLOW-1B 审批流程',
  biz_type: 'TASK',
  biz_no: '',
  instance_status: 'PENDING',
  initiator_id: '01a18171-e94a-7537-b822-d75e6566845c',
  initiator_name: 'Henry.Liao',
  started_at: '2026-11-01T06:00:00Z',
  ended_at: null,
  node_key: 'n_task',
  node_name: '催货',
  round: 1,
  node_status: 'ACTIVE',
  on_complete: 'APPROVE',
  on_reject: 'TERMINATE',
  on_cancel: 'TERMINATE',
  approval: {
    instance_id: '01a195b1-70a1-77b2-8765-0cdb66e6d59c',
    instance_no: 'FLW-20261101-3D798C31',
    status: 'PENDING',
    pending_approvers: ['王五'],
  },
};

/** 审批通过之后同一接口：`status=APPROVED`、待办人清空、节点也已推进 */
const APPROVED_FLOW: TaskFlowDetail = {
  ...PENDING_FLOW,
  node_status: 'APPROVED',
  approval: {
    instance_id: '01a195b1-70a1-77b2-8765-0cdb66e6d59c',
    instance_no: 'FLW-20261101-3D798C31',
    status: 'APPROVED',
    pending_approvers: [],
  },
};

/** DIRECT 节点完成：`approval` 整段为 `null`（字段仍在，不省略） */
const DIRECT_FLOW: TaskFlowDetail = {
  ...PENDING_FLOW,
  on_complete: 'DIRECT',
  node_status: 'APPROVED',
  approval: null,
};

/** 只有一个定位函数：任务状态 + approval 段 → 后缀 */
function suffixOf(flow: TaskFlowDetail | null, taskStatus: string): string | null {
  return flowApprovalSuffix(taskStatus, flow?.approval);
}

// ---------------------------------------------------------------- 显示的那一条

test('PENDING + 王五 → 「✓ 审批中（王五）」（设计稿原文）', () => {
  assert.equal(suffixOf(PENDING_FLOW, 'DONE'), '✓ 审批中（王五）');
  // 栏里「已完成」徽标 + 这段后缀 = 设计稿的「已完成 ✓ 审批中（王五）」
  assert.equal(`已完成 ${suffixOf(PENDING_FLOW, 'DONE')}`, '已完成 ✓ 审批中（王五）');
});

test('多人格式钉死：2 人逐个列、≥3 人折叠成「首人 等 N 人」', () => {
  const two: FlowApprovalState = { status: 'PENDING', pending_approvers: ['王五', '李四'] };
  assert.equal(flowApprovalSuffix('DONE', two), '✓ 审批中（王五、李四）');

  const three: FlowApprovalState = { status: 'PENDING', pending_approvers: ['王五', '李四', '赵六'] };
  assert.equal(flowApprovalSuffix('DONE', three), '✓ 审批中（王五 等 3 人）');
  // 折叠阈值就是 2：这条断言让「改成 3 人内联」必须先改测试
  assert.equal(FLOW_APPROVAL_INLINE_MAX, 2);
});

test('待办人去重、去空白、丢空串后仍按规则渲染', () => {
  // 并行节点可能给同一人派多条待办（后端已去重，这里兜脏数据）
  assert.equal(
    flowApprovalSuffix('DONE', { status: 'PENDING', pending_approvers: ['王五', '王五'] }),
    '✓ 审批中（王五）',
  );
  assert.equal(
    flowApprovalSuffix('DONE', { status: 'PENDING', pending_approvers: [' 王五 ', '', '李四'] }),
    '✓ 审批中（王五、李四）',
  );
});

// ---------------------------------------------------------------- 不显示的三条（卡面点名）

test('APPROVED → 不显示「审批中」', () => {
  assert.equal(suffixOf(APPROVED_FLOW, 'DONE'), null);
});

test('approval = null（DIRECT 节点完成）→ 不显示「审批中」', () => {
  assert.equal(suffixOf(DIRECT_FLOW, 'DONE'), null);
});

test('approval = null 且节点仍 ACTIVE → 也不显示（不许拿 node_status 猜）', () => {
  // 1B 之前的老路：`node_status=ACTIVE + task.status=DONE` → 猜「在等审批」。判据只认 approval。
  const guessed: TaskFlowDetail = { ...DIRECT_FLOW, node_status: 'ACTIVE' };
  assert.equal(suffixOf(guessed, 'DONE'), null);
});

// ---------------------------------------------------------------- 其它边界

test('PENDING 但待办人为空 → 不显示（没人可等）', () => {
  assert.equal(
    flowApprovalSuffix('DONE', { status: 'PENDING', pending_approvers: [] }),
    null,
  );
});

test('任务未 DONE（REVIEW / DOING）→ 即使审批 PENDING 也不显示', () => {
  // REVIEW 态等的是**验收审批**，栏里已有「待验收」徽标，再挂「审批中」是重复信息
  for (const status of ['REVIEW', 'DOING', 'TODO', 'CANCELLED']) {
    assert.equal(suffixOf(PENDING_FLOW, status), null, `${status} 不应显示审批后缀`);
  }
});

test('其它终态（REJECTED / CANCELLED / WITHDRAWN）→ 不显示', () => {
  for (const status of ['REJECTED', 'CANCELLED', 'WITHDRAWN']) {
    const approval: FlowApprovalState = { status, pending_approvers: ['王五'] };
    assert.equal(flowApprovalSuffix('DONE', approval), null, `${status} 不应显示审批后缀`);
  }
});

test('flow 整段为 null（手工任务）→ 不显示', () => {
  assert.equal(suffixOf(null, 'DONE'), null);
});

test('样例的 approval 字段恒在：APPROVED 时是对象、DIRECT 时是 null（不是缺字段）', () => {
  assert.ok('approval' in PENDING_FLOW);
  assert.equal(PENDING_FLOW.approval?.status, 'PENDING');
  assert.deepEqual(PENDING_FLOW.approval?.pending_approvers, ['王五']);
  // 终态清空待办人（后端规则），前端据此不再显示
  assert.equal(APPROVED_FLOW.approval?.status, 'APPROVED');
  assert.deepEqual(APPROVED_FLOW.approval?.pending_approvers, []);
  assert.equal(DIRECT_FLOW.approval, null);
});

/**
 * 签名模式单测（P3-06）：新建节点默认值 + 节点副标题文案。
 *
 * 运行：`pnpm test:unit`（`tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 卡面 ⑤ 要求「前端 `graph.ts` 新建节点默认值的单测」—— 这里钉三件事：
 *
 * 1. `makeNode('APPROVAL')` 必须**显式写入** `approval_mode = 'ANY'`（不填 = 或签的新语义，
 *    但库里不留 NULL 让人猜是「没配」还是「或签」）；
 * 2. `makeNode('COUNTERSIGN')` 仍是 `ALL`（会签节点的历史默认不变）；
 * 3. 其余节点（任务 / 通知 / 网关 …）不许带 `approval_mode`。
 *
 * 另外顺带钉住副标题：`APPROVAL` 也要显示签名模式（以前只给 `COUNTERSIGN` 显示，
 * 用户在画布上看不出这个审批节点到底是或签还是全员签）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { makeNode } from '../../src/views/business/flow/graph.ts';
import { nodeSubLabel } from '../../src/views/business/flow/components/flow-node-view.ts';
import type { FlowNode, FlowNodeType } from '../../src/api/business/flow-api';

/** 造一个节点（走真实的 makeNode，不手搓对象） */
function made(type: FlowNodeType): FlowNode {
  return makeNode(type, { x: 200, y: 120 }, []);
}

test('新建审批节点默认写 ANY（不填=或签，但要显式落库）', () => {
  assert.equal(made('APPROVAL').approval_mode, 'ANY');
});

test('新建会签节点仍是 ALL（历史默认不变）', () => {
  assert.equal(made('COUNTERSIGN').approval_mode, 'ALL');
});

test('非审批类节点不带签名模式', () => {
  for (const type of ['START', 'END', 'CONDITION', 'PARALLEL', 'CC', 'TASK_NODE'] as FlowNodeType[]) {
    assert.equal(made(type).approval_mode, null, `${type} 不该带 approval_mode`);
  }
});

test('副标题：APPROVAL 也显示签名模式（或签 / 全员签 / 按比例）', () => {
  const base = made('APPROVAL');
  assert.equal(nodeSubLabel({ ...base, approval_mode: 'ANY' }), '或签');
  assert.equal(nodeSubLabel({ ...base, approval_mode: 'ALL' }), '全部同意');
  assert.equal(nodeSubLabel({ ...base, approval_mode: 'RATIO', approve_ratio: 0.6 }), '按比例 60%');
  // 老数据（迁移前遗留/手工插入）没模式时，不显示模式，回落到类型说明
  assert.equal(nodeSubLabel({ ...base, approval_mode: null }), '');
});

test('副标题：会签节点文案不变（回归）', () => {
  const base = made('COUNTERSIGN');
  assert.equal(nodeSubLabel({ ...base, approval_mode: 'ALL' }), '全部同意');
  assert.equal(nodeSubLabel({ ...base, approval_mode: 'ANY' }), '或签');
  assert.equal(nodeSubLabel({ ...base, approval_mode: 'RATIO', approve_ratio: 0.5 }), '按比例 50%');
});

test('副标题：没配审批人优先于模式（缺人更该被看见）', () => {
  const base = made('APPROVAL');
  assert.equal(nodeSubLabel({ ...base, approval_mode: 'ANY', assignee_type: null }), '未配置审批人');
});

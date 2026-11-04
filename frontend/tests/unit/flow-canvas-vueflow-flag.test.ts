/**
 * 画布引擎开关 + 节点视图模型单测（VUE-FLOW-1）。
 *
 * 运行：`pnpm test:unit`。
 *
 * 两组用例：
 * 1. **flag 优先级 / 回退开关**：`?canvas=` > `VITE_FLOW_CANVAS` > 默认 vueflow（VUE-FLOW-3 起）。
 *    默认必须是 vueflow（新画布已全量验收）；但 `?canvas=native` 与
 *    `VITE_FLOW_CANVAS=native` 两条回退路径必须一直可用 —— 线上出问题时要能当场切回，
 *    所以「回退可用」和「默认值」在这里各有一条断言，改默认值时不会顺手把回退弄丢。
 * 2. **节点视图文案**：Vue Flow 画布的卡片文案（会签模式 / 缺审批人 / 任务完成方式 /
 *    运行状态叠加 / 类型色 class）是**从老画布逐字搬过来的**，这里把期望值写死，
 *    防止「搬的时候少了一行 else-if」这种只在真机才看得见的漂移。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { FLOW_CANVAS_ENV_VAR, engineFromSearch, normalizeFlowCanvasEngine, resolveFlowCanvasEngine } from '../../src/views/business/flow/components/flow-canvas-flag.ts';
import { NODE_GLYPH, buildNodeView, nodeSizeOf } from '../../src/views/business/flow/components/flow-node-view.ts';
import type { FlowNode } from '../../src/api/business/flow-api.ts';

/** 造节点（默认 APPROVAL，可按需覆盖字段） */
function node(patch: Partial<FlowNode> = {}): FlowNode {
  return {
    key: 'n1',
    name: '审批人',
    type: 'APPROVAL',
    x: 0,
    y: 0,
    sort_order: 0,
    assignee_type: 'INITIATOR_SUPERIOR',
    superior_levels: 1,
    form_var_key: null,
    approval_mode: null,
    approve_ratio: null,
    reject_veto: true,
    empty_assignee: 'SKIP',
    self_approval: 'SKIP',
    allow_robot: false,
    join_mode: 'AND',
    fork_mode: 'AND',
    timeout_hours: null,
    timeout_action: 'NONE',
    remind_before_hours: null,
    config: {},
    assignees: [],
    ...patch,
  } as FlowNode;
}

// ==================== ① flag 优先级 ====================

test('① 默认引擎是 vueflow（env / 参数都没给）', () => {
  assert.equal(resolveFlowCanvasEngine({ search: '', env: undefined }), 'vueflow');
});

test('② env 生效：VITE_FLOW_CANVAS=vueflow', () => {
  assert.equal(resolveFlowCanvasEngine({ search: '', env: 'vueflow' }), 'vueflow');
  assert.equal(resolveFlowCanvasEngine({ search: '', env: 'NATIVE' }), 'native');
});

test('③ URL 参数优先于 env（运维灰度后，开发者仍能单页 A/B）', () => {
  assert.equal(resolveFlowCanvasEngine({ search: '?canvas=native', env: 'vueflow' }), 'native');
  assert.equal(resolveFlowCanvasEngine({ search: '?canvas=vueflow', env: 'native' }), 'vueflow');
  assert.equal(resolveFlowCanvasEngine({ search: '?foo=1&canvas=vueflow', env: 'native' }), 'vueflow');
});

test('④ 非法值不生效（拼错参数不该把画布换掉）；回退到 native 一定有两条路', () => {
  assert.equal(resolveFlowCanvasEngine({ search: '?canvas=vufelow', env: 'vueflow' }), 'vueflow');
  assert.equal(resolveFlowCanvasEngine({ search: '?canvas=', env: 'nonsense' }), 'vueflow');
  // 回退路径 ①：URL 参数（不依赖 env、不改代码）
  assert.equal(resolveFlowCanvasEngine({ search: '?canvas=native', env: undefined }), 'native');
  // 回退路径 ②：构建期 env（整个环境一起回退）
  assert.equal(resolveFlowCanvasEngine({ search: '', env: 'native' }), 'native');
  assert.equal(normalizeFlowCanvasEngine(undefined), null);
  assert.equal(normalizeFlowCanvasEngine(42), null);
  assert.equal(normalizeFlowCanvasEngine('vue-flow'), 'vueflow');
});

test('⑤ 参数解析：编码 / 别名 / 大小写', () => {
  assert.equal(engineFromSearch('?canvas=VueFlow'), 'vueflow');
  assert.equal(engineFromSearch('canvas=svg'), 'native');
  assert.equal(engineFromSearch('?other=1'), null);
  assert.equal(FLOW_CANVAS_ENV_VAR, 'VITE_FLOW_CANVAS');
});

// ==================== ② 节点视图 ====================

test('⑥ 九种节点都有图标 + 尺寸与 graph.ts 一致', () => {
  const types = ['START', 'APPROVAL', 'COUNTERSIGN', 'CONDITION', 'PARALLEL', 'CC', 'END', 'TASK_NODE', 'NOTIFY_NODE'];
  for (const type of types) {
    assert.ok(NODE_GLYPH[type], `${type} 缺图标`);
  }
  // 尺寸口径只能来自 graph.ts（连线锚点按它算），这里抽查三种代表
  assert.deepEqual(nodeSizeOf('APPROVAL'), { w: 150, h: 64 });
  assert.deepEqual(nodeSizeOf('START'), { w: 96, h: 44 });
  assert.deepEqual(nodeSizeOf('CONDITION'), { w: 132, h: 84 });
  assert.deepEqual(nodeSizeOf('UNKNOWN_TYPE'), { w: 150, h: 64 }, '未知类型必须兜底成审批尺寸');
});

test('⑦ 副标题：九种类型逐条对齐老画布文案', () => {
  const cases: Array<[FlowNode, string]> = [
    [node({ type: 'START', name: '开始' }), '入口'],
    [node({ type: 'END', name: '结束' }), '出口'],
    [node({ type: 'APPROVAL' }), ''],
    [node({ type: 'COUNTERSIGN', approval_mode: 'ALL' }), '全部同意'],
    [node({ type: 'COUNTERSIGN', approval_mode: 'ANY' }), '或签'],
    [node({ type: 'COUNTERSIGN', approval_mode: 'RATIO', approve_ratio: 0.75 }), '按比例 75%'],
    [node({ type: 'APPROVAL', assignee_type: null }), '未配置审批人'],
    [node({ type: 'TASK_NODE', assignee_type: null }), '未配置负责人'],
    [node({ type: 'TASK_NODE', config: { task: { on_complete: 'VERIFY' } } }), '需验收'],
    [node({ type: 'NOTIFY_NODE', assignee_type: null }), '未配置审批人'],
    [node({ type: 'CONDITION', assignee_type: null }), '分支网关'],
    [node({ type: 'PARALLEL', assignee_type: null, fork_mode: 'OR' }), '或分叉'],
    [node({ type: 'CC', assignee_type: 'INITIATOR_SUPERIOR' }), '只读知会'],
  ];
  for (const [input, expected] of cases) {
    assert.equal(buildNodeView(input).subLabel, expected, `${input.type} 副标题不符`);
  }
});

test('⑧ 运行状态叠加到副标题 + 状态点', () => {
  const view = buildNodeView(node({ type: 'CONDITION', assignee_type: null }), { status: { n1: 'ACTIVE' } });
  assert.equal(view.subLabel, '分支网关 · 处理中');
  assert.equal(view.hasStatus, true);
  assert.equal(view.statusMark, '');
  assert.equal(view.statusTone, 'active');
  assert.equal(view.cls['is-active'], true);

  const approved = buildNodeView(node(), { status: { n1: 'APPROVED' } });
  assert.equal(approved.statusMark, '✓');
  assert.equal(approved.statusTone, 'approved');

  // START/END 没有别的副标题时，状态直接当副标题
  const start = buildNodeView(node({ type: 'START', name: '开始' }), { status: { n1: 'WAITING' } });
  assert.equal(start.subLabel, '入口 · 待处理');
  assert.equal(start.cls['is-skipped'], true);
});

test('⑨ class 集合：选中 / 校验 / 紧凑 / 类型色 一个不落', () => {
  const view = buildNodeView(node({ type: 'START', name: '开始' }), {
    selectedKey: 'n1',
    issueKeys: new Set(['n1']),
    connectTargetKey: 'n1',
  });
  assert.equal(view.cls['is-selected'], true);
  assert.equal(view.cls['has-issue'], true);
  assert.equal(view.cls['is-connect-target'], true);
  // 高 44 的胶囊要切紧凑排版（图标缩到 22×22、副标题隐藏）
  assert.equal(view.cls['is-compact'], true);
  assert.equal(view.cls['is-start'], true);
  assert.equal(view.w, 96);

  const approval = buildNodeView(node());
  assert.equal(approval.cls['is-compact'], false);
  assert.equal(approval.cls['is-approval'], true);
  assert.equal(approval.w, 150);
});

test('⑩ 主标题：name 为空时用类型中文名兜底', () => {
  assert.equal(buildNodeView(node({ type: 'CONDITION', name: '' })).label, '条件');
  assert.equal(buildNodeView(node({ type: 'APPROVAL', name: '  财务复核  ' })).label, '财务复核');
});

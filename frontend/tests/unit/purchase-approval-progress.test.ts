/**
 * P1-FIX-2 单测：采购订单详情显示审批进度（照抄收货单 / 应付单的 `FlowProgress`）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * # 为什么这几条必须测
 *
 * 这一批改动全在「有 / 没有 / 拉不到」三种分支里，`vue-tsc` 抓不到，e2e 只看得见
 * 一张正常单的样子：
 *
 * | 写错了会怎样 | 钉它的用例 |
 * |---|---|
 * | 详情有实例 id 却不拉节点 → 老板要的「审批情况」还是看不到 | ① |
 * | 草稿单（无实例）也画个空区块 / 空链接 | ② |
 * | 审批中心抽风 → 详情页整块 `detailError`，单据都看不了 | ③ |
 * | 节点状态配色错（进行中当未到达、驳回当通过） | ④ |
 * | 审批人明细丢时间 / 丢意见 → 追溯不了「谁几点说了什么」 | ⑤ |
 * | 换一张单还留着上一张的节点（ref 没重置） | ⑥ |
 *
 * fixture 形状照 `GET /business/flow/instance/{id}`（`nodes[]` + `timeline[]`）抄，
 * 不是编的：节点 `node_key/node_name/node_type/round/status`、任务
 * `assignee_name/status/comment/completed_at`。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import type { FlowNodeProgress, FlowTaskItem } from '@/api/business/flow-api';
import {
  APPROVAL_PROGRESS_TITLE,
  EMPTY_APPROVAL_PROGRESS,
  approvalFallbackOf,
  hasApprovalProgress,
  loadApprovalProgress,
  mapApprovalNodes,
  mapApprovalTasks,
} from '@/views/business/erp/shared/approval-progress';
import {
  escalateReasonLabel,
  escalationHint,
  groupProgressTasks,
  orderProgressNodes,
  progressNodeClass,
  progressNodeLabel,
  progressTaskClass,
  progressTaskLabel,
  type FlowProgressNode,
} from '@/views/business/flow/components/flow-progress-tasks';

// ==================== fixture（PO-20261031-000002 的真实形状） ====================

/** 节点进度（开始 ✓ / 金额分级① ✓ / 部门主管审批 ◉ 进行中） */
const NODES: FlowNodeProgress[] = [
  {
    node_key: 'n_start',
    node_name: '开始',
    node_type: 'START',
    round: 1,
    status: 'APPROVED',
    approval_mode: 'SINGLE',
    approve_ratio: 0,
    approver_count: 0,
    approved_count: 0,
    rejected_count: 0,
    is_auto_passed: true,
    taken_edge_key: '',
    activated_at: '2026-11-01T06:00:00Z',
    ended_at: '2026-11-01T06:00:00Z',
    duration_ms: 0,
  },
  {
    node_key: 'n_amount_1',
    node_name: '金额分级①（< 1 万）',
    node_type: 'APPROVAL',
    round: 1,
    status: 'APPROVED',
    approval_mode: 'SINGLE',
    approve_ratio: 0,
    approver_count: 1,
    approved_count: 1,
    rejected_count: 0,
    is_auto_passed: true,
    taken_edge_key: 'e_amount_low',
    activated_at: '2026-11-01T06:00:00Z',
    ended_at: '2026-11-01T06:00:00Z',
    duration_ms: 0,
  },
  {
    node_key: 'n_dept',
    node_name: '部门主管审批',
    node_type: 'APPROVAL',
    round: 1,
    status: 'ACTIVE',
    approval_mode: 'OR',
    approve_ratio: 0,
    approver_count: 3,
    approved_count: 1,
    rejected_count: 0,
    is_auto_passed: false,
    taken_edge_key: '',
    activated_at: '2026-11-01T06:23:00Z',
    ended_at: null,
    duration_ms: null,
  },
];

/** 时间线上的三个人（Henry.Liao 已批准，另外两个待处理） */
const TIMELINE: FlowTaskItem[] = [
  {
    id: 'task-1',
    instance_id: 'inst-1',
    instance_no: 'FLW-20261031-000002',
    title: '采购单 PO-20261031-000002',
    flow_name: '采购订单审批',
    initiator_name: 'Henry.Liao',
    current_node_name: '部门主管审批',
    arrived_at: '2026-11-01T06:23:00Z',
    node_key: 'n_dept',
    round: 1,
    task_kind: 'APPROVAL',
    assignee_id: 'u-henry',
    assignee_name: 'Henry.Liao',
    assignee_source: 'ROLE',
    status: 'APPROVED',
    instance_status: 'RUNNING',
    is_added_sign: false,
    form_data: {},
    is_self_approval: false,
    is_auto: false,
    comment: '同意采购，价格合理',
    transferred_from: '',
    created_at: '2026-11-01T06:23:00Z',
    completed_at: '2026-11-01T06:23:00Z',
  },
  {
    id: 'task-2',
    instance_id: 'inst-1',
    instance_no: 'FLW-20261031-000002',
    title: '采购单 PO-20261031-000002',
    flow_name: '采购订单审批',
    initiator_name: 'Henry.Liao',
    current_node_name: '部门主管审批',
    arrived_at: '2026-11-01T06:23:00Z',
    node_key: 'n_dept',
    round: 1,
    task_kind: 'APPROVAL',
    assignee_id: 'u-laoliu',
    assignee_name: '老六',
    assignee_source: 'ROLE',
    status: 'PENDING',
    instance_status: 'RUNNING',
    is_added_sign: false,
    form_data: {},
    is_self_approval: false,
    is_auto: false,
    comment: '',
    transferred_from: '',
    created_at: '2026-11-01T06:23:00Z',
    completed_at: null,
  },
  {
    id: 'task-3',
    instance_id: 'inst-1',
    instance_no: 'FLW-20261031-000002',
    title: '采购单 PO-20261031-000002',
    flow_name: '采购订单审批',
    initiator_name: 'Henry.Liao',
    current_node_name: '部门主管审批',
    arrived_at: '2026-11-01T06:23:00Z',
    node_key: 'n_dept',
    round: 1,
    task_kind: 'APPROVAL',
    assignee_id: 'u-admin',
    assignee_name: '超级管理员',
    assignee_source: 'ROLE',
    status: 'PENDING',
    instance_status: 'RUNNING',
    is_added_sign: false,
    form_data: {},
    is_self_approval: true,
    is_auto: false,
    comment: '',
    transferred_from: '',
    created_at: '2026-11-01T06:23:00Z',
    completed_at: null,
  },
];

/** 假 `GET /business/flow/instance/{id}`（记录调用次数，供「拉了几次」断言） */
function fakeInstanceApi(payload: {
  nodes?: FlowNodeProgress[] | null;
  timeline?: FlowTaskItem[] | null;
} = { nodes: NODES, timeline: TIMELINE }) {
  const calls: string[] = [];
  return {
    calls,
    fetchInstance: async (id: string) => {
      calls.push(id);
      return { nodes: payload.nodes, timeline: payload.timeline };
    },
  };
}

// ==================== ① 有实例 id → 拉节点 → 渲染 FlowProgress ====================

test('① 有 approval_instance_id → 拉一次实例 → 拿到 3 个节点，进度区块该渲染', async () => {
  const api = fakeInstanceApi();

  const progress = await loadApprovalProgress('inst-1', api.fetchInstance);

  // 只拉一次（详情抽屉每次打开一次，别重复打同一个接口）
  assert.deepEqual(api.calls, ['inst-1']);
  // 模板判据：有节点 → 渲染「审批进度」+ FlowProgress
  assert.equal(hasApprovalProgress(progress), true);
  assert.equal(APPROVAL_PROGRESS_TITLE, '审批进度');
  // 节点按图顺序、中文名带出来（`node_name` 是唯一带中文名的字段）
  assert.deepEqual(
    progress.nodes.map((node) => node.name),
    ['开始', '金额分级①（< 1 万）', '部门主管审批'],
  );
  assert.deepEqual(
    progress.nodes.map((node) => node.status),
    ['APPROVED', 'APPROVED', 'ACTIVE'],
  );
  // 审批人明细也一起回来了（没有它就只有一根条，看不到谁批的）
  assert.equal(progress.tasks.length, 3);
});

// ==================== ② 无实例 id → 不渲染 ====================

test('② 没有 approval_instance_id（草稿 / 已打回）→ 一次都不拉、进度与降级链接都不渲染', async () => {
  const api = fakeInstanceApi();

  const progress = await loadApprovalProgress(null, api.fetchInstance);
  const fallback = approvalFallbackOf({
    status: 'DRAFT',
    approval_instance_id: null,
    approval_instance_no: null,
  });

  assert.deepEqual(api.calls, [], '没有实例 id 就不该发请求');
  assert.deepEqual(progress, EMPTY_APPROVAL_PROGRESS);
  assert.equal(hasApprovalProgress(progress), false, '无节点 → 不渲染进度区块');
  assert.equal(fallback, null, '无实例 id → 连降级链接都不该有');
});

// ==================== ③ 拉节点失败 → 降级单号链接，不报错 ====================

test('③ 拉节点抛错（权限 / 实例已归档）→ 不抛异常、退化成单号链接', async () => {
  const failing = async (): Promise<never> => {
    throw new Error('403 无权限查看审批实例');
  };

  // 不 reject，就是「没节点」——详情页不整块失败的前提
  const progress = await loadApprovalProgress('inst-1', failing);

  assert.deepEqual(progress, EMPTY_APPROVAL_PROGRESS);
  assert.equal(hasApprovalProgress(progress), false);

  // 降级文案：审批中的单说「审批中 FLW-xxx」，其它状态说「审批单 FLW-xxx」
  assert.deepEqual(
    approvalFallbackOf({
      status: 'SUBMITTED',
      approval_instance_id: 'inst-1',
      approval_instance_no: 'FLW-20261031-000002',
    }),
    { label: '审批中', no: 'FLW-20261031-000002' },
  );
  assert.deepEqual(
    approvalFallbackOf({
      status: 'APPROVED',
      approval_instance_id: 'inst-1',
      approval_instance_no: 'FLW-20261031-000002',
    }),
    { label: '审批单', no: 'FLW-20261031-000002' },
  );
  // 连单号都没回填时用实例 id 兜底（不能出现「审批中」后面空白）
  assert.deepEqual(
    approvalFallbackOf({ status: 'SUBMITTED', approval_instance_id: 'inst-1', approval_instance_no: null }),
    { label: '审批中', no: 'inst-1' },
  );
});

// ==================== ④ 节点状态映射 ====================

test('④ 节点状态映射：APPROVED→已通过、ACTIVE→处理中、PENDING→未到达，未知状态原样显示', () => {
  assert.equal(progressNodeLabel('APPROVED'), '已通过');
  assert.equal(progressNodeLabel('ACTIVE'), '处理中');
  assert.equal(progressNodeLabel('PENDING'), '未到达');
  assert.equal(progressNodeLabel('REJECTED'), '已驳回');
  assert.equal(progressNodeLabel('WHAT_IS_THIS'), 'WHAT_IS_THIS', '未知状态不许显示空白');

  assert.deepEqual(progressNodeClass('APPROVED'), {
    'is-done': true,
    'is-skipped': false,
    'is-active': false,
    'is-rejected': false,
    'is-pending': false,
  });
  assert.deepEqual(progressNodeClass('ACTIVE'), {
    'is-done': false,
    'is-skipped': false,
    'is-active': true,
    'is-rejected': false,
    'is-pending': false,
  });
  // 空状态按「未到达」画（老数据 node.status 可能是空串）
  assert.equal(progressNodeClass('')['is-pending'], true);

  // 任务侧状态是另一套（多 TRANSFERRED/DELEGATED，没有 ACTIVE）
  assert.equal(progressTaskLabel('APPROVED'), '已同意');
  assert.equal(progressTaskLabel('PENDING'), '待处理');
  assert.deepEqual(progressTaskClass('APPROVED'), {
    'is-approved': true,
    'is-rejected': false,
    'is-pending': false,
    'is-closed': false,
  });
  assert.equal(progressTaskClass('TERMINATED')['is-closed'], true);
});

// ==================== ⑤ 审批人 + 时间 + 意见 ====================

test('⑤ 三个审批人挂在「部门主管审批」下：姓名 / 状态 / 时间 / 意见都在', () => {
  const tasks = mapApprovalTasks(TIMELINE);
  const groups = groupProgressTasks(mapApprovalNodes(NODES), tasks);

  // 「开始」「金额分级①」没有待办行 → 不出组（不给空标题）
  assert.deepEqual(
    groups.map((group) => group.name),
    ['部门主管审批'],
  );
  const [dept] = groups;
  assert.equal(dept.tasks.length, 3);
  assert.deepEqual(
    dept.tasks.map((task) => [task.assignee_name, task.status]),
    [
      ['Henry.Liao', 'APPROVED'],
      ['老六', 'PENDING'],
      ['超级管理员', 'PENDING'],
    ],
  );
  // 已批的人有时间 + 意见；待处理的人退回「到岗时间」也要有（否则看不出等多久了）
  assert.equal(dept.tasks[0].at, '2026-11-01T06:23:00Z');
  assert.equal(dept.tasks[0].comment, '同意采购，价格合理');
  assert.equal(dept.tasks[1].at, '2026-11-01T06:23:00Z', '待处理时间 = 到岗时间');
  assert.equal(dept.tasks[1].comment, '');
  // 自审标记照样带出来（超管自审要能看见）
  assert.equal(dept.tasks[2].is_self_approval, true);
});

test('⑤-补 节点表里没有的节点（退回后的旧轮次）也要显示审批人，不许丢', () => {
  const oldRound: FlowTaskItem = {
    ...TIMELINE[0],
    id: 'task-old',
    node_key: 'n_manager',
    round: 1,
    current_node_name: '经理审批',
    status: 'REJECTED',
    comment: '金额要重新谈',
  };
  const tasks = mapApprovalTasks([oldRound]);
  // 节点表只给当前轮（n_dept），旧轮次的 n_manager 不在里面
  const groups = groupProgressTasks(mapApprovalNodes(NODES), tasks);

  assert.equal(groups.length, 1);
  assert.equal(groups[0].name, '经理审批', '会落到任务自带的 node_name');
  assert.equal(groups[0].tasks[0].status, 'REJECTED');
});

// ==================== ⑥ 数据刷新（提交审批后重新打开详情） ====================
test('⑥ 提交审批后再开详情：换了实例 id 就拿到新节点；没有实例就清空', async () => {
  // 第一次：DRAFT（无实例）→ 什么都不显示
  const before = await loadApprovalProgress(null, fakeInstanceApi().fetchInstance);
  assert.equal(hasApprovalProgress(before), false);

  // 提交审批后：单据回填 approval_instance_id → 同一个 loader 拿到节点
  const submitted = await loadApprovalProgress('inst-1', fakeInstanceApi().fetchInstance);
  assert.equal(hasApprovalProgress(submitted), true);
  assert.equal(submitted.nodes.length, 3);

  // 审批再走一步（节点多了 / 状态变了）→ 重新拉到的就是新状态（不是缓存）
  const advanced = await loadApprovalProgress('inst-1', async () => ({
    nodes: [
      ...NODES,
      {
        ...NODES[2],
        node_key: 'n_finance',
        node_name: '财务复核',
        status: 'PENDING',
      },
    ],
    timeline: TIMELINE,
  }));
  assert.equal(advanced.nodes.length, 4);
  assert.equal(advanced.nodes[3].name, '财务复核');
  assert.equal(advanced.nodes[3].status, 'PENDING');

  // 再开一张草稿单 → 必须回到空（ref 不能留上一张的节点）
  const next = await loadApprovalProgress(null, fakeInstanceApi().fetchInstance);
  assert.deepEqual(next, EMPTY_APPROVAL_PROGRESS);
});

// ==================== ⑦ 显示顺序（真接口的坑） ====================

test('⑦ 后端节点按 node_key 排出来的「开始」在最后 → 进度条要 START 第一、END 最后', () => {
  // 实测形状：`FLW-20261101-02524DBF` 回的是 c_low / n_dept / s / e（开始排在第三位）
  const raw: FlowProgressNode[] = [
    { node_key: 'c_low', name: '金额分级①（< 1 万）', type: 'CONDITION', round: 1, status: 'APPROVED' },
    { node_key: 'n_dept', name: '部门主管审批', type: 'APPROVAL', round: 1, status: 'APPROVED' },
    { node_key: 's', name: '开始', type: 'START', round: 1, status: 'APPROVED' },
    { node_key: 'e', name: '结束', type: 'END', round: 1, status: 'APPROVED' },
  ];

  assert.deepEqual(
    orderProgressNodes(raw).map((node) => node.node_key),
    ['s', 'c_low', 'n_dept', 'e'],
  );

  // 第二轮（退回后）整体排在后面，第二轮里的 START 也不能插到第一轮前面
  const twoRounds: FlowProgressNode[] = [
    { node_key: 'n_dept', name: '部门主管审批', type: 'APPROVAL', round: 2, status: 'ACTIVE' },
    { node_key: 's', name: '开始', type: 'START', round: 1, status: 'APPROVED' },
    { node_key: 'n_dept', name: '部门主管审批', type: 'APPROVAL', round: 1, status: 'REJECTED' },
  ];
  assert.deepEqual(
    orderProgressNodes(twoRounds).map((node) => `${node.node_key}#${node.round}`),
    ['s#1', 'n_dept#1', 'n_dept#2'],
  );

  // 不改入参（props 是只读的，排序不能原地改）
  assert.deepEqual(raw.map((node) => node.node_key), ['c_low', 'n_dept', 's', 'e']);
});

// ==================== P3-02：升级留痕（escalate_reason / escalated_to_role） ====================

test('⑯ 升级派下来的待办带「已升级至上级角色」提示（自审跳过）', () => {
  const [task] = mapApprovalTasks([
    {
      node_key: 'n_sales',
      round: 1,
      current_node_name: '销售主管审批',
      assignee_id: 'u-2',
      assignee_name: '李四',
      status: 'PENDING',
      comment: '',
      is_auto: false,
      is_self_approval: false,
      escalated_from_role: 'SALES_CLERK',
      escalate_reason: 'SKIP_SELF_APPROVAL',
      escalated_to_role: '销售主管',
      created_at: '2026-11-08T10:00:00Z',
      completed_at: null,
    } as FlowTaskItem,
  ]);
  assert.equal(task.escalate_reason, 'SKIP_SELF_APPROVAL');
  assert.equal(task.escalated_to_role, '销售主管');
  assert.equal(
    escalationHint(task),
    '⚠ 因自审跳过，已升级至「销售主管」审批（原角色 SALES_CLERK）',
  );
});

test('⑰ 角色 0 人升级：文案区分「角色无成员」；未升级的行没有提示', () => {
  const tasks = mapApprovalTasks([
    {
      node_key: 'n_sales',
      round: 1,
      current_node_name: '销售主管审批',
      assignee_id: 'u-3',
      assignee_name: '王五',
      status: 'PENDING',
      comment: '',
      escalate_reason: 'NO_MEMBER',
      escalated_from_role: 'SALES_CLERK',
      escalated_to_role: '销售总监',
      created_at: '2026-11-08T10:00:00Z',
      completed_at: null,
    } as FlowTaskItem,
    {
      node_key: 'n_sales',
      round: 1,
      current_node_name: '销售主管审批',
      assignee_id: 'u-4',
      assignee_name: '赵六',
      status: 'APPROVED',
      comment: '',
      created_at: '2026-11-08T10:00:00Z',
      completed_at: '2026-11-08T11:00:00Z',
    } as FlowTaskItem,
  ]);
  assert.equal(escalationHint(tasks[0]), '⚠ 因角色无成员，已升级至「销售总监」审批（原角色 SALES_CLERK）');
  assert.equal(escalationHint(tasks[1]), '', '普通待办不显示升级提示');
});

test('⑱ 老后端没有升级字段 / 目标角色已删：不崩、给出兜底文案', () => {
  const [task] = mapApprovalTasks([
    {
      node_key: 'n_sales',
      round: 1,
      current_node_name: '销售主管审批',
      assignee_id: 'u-2',
      assignee_name: '李四',
      status: 'PENDING',
      comment: '',
      escalate_reason: 'SKIP_SELF_APPROVAL',
      created_at: '2026-11-08T10:00:00Z',
      completed_at: null,
    } as FlowTaskItem,
  ]);
  assert.equal(task.escalated_to_role, '');
  assert.equal(escalationHint(task), '⚠ 因自审跳过，已升级至上级角色审批');
  assert.equal(escalateReasonLabel(undefined), '因无可用审批人');
});

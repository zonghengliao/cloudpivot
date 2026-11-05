/**
 * C2 单测：四个单据（采购 / 收货 / 应付 / 凭证 / 调拨）的审批进度统一。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * # 这一批改的是什么
 *
 * | 页面 | 改前 | 改后 |
 * |---|---|---|
 * | 收货单 | `FlowProgress :nodes` （看不到谁审的） | `:nodes` + `:tasks` |
 * | 应付单 | 同上 | 同上 |
 * | 凭证 | 只有「红冲审批中」徽标 + 撤回按钮 | 详情底部多一节「审批进度」（节点 + 审批人明细） |
 * | 调拨单 | 自己画的时间线（`.timeline__item`） | 全站 `FlowProgress`（+ 一行「提交人 / 时间 / 单号」） |
 *
 * # 命名口径（卡 t_3cc4caf2 定死，别再漂）
 *
 * 凭证的「红冲审批进度」并进了**统一状态** `approvalProgress`（P1-11：收货 / 应付 / 调拨 / 凭证
 * 共用同一个 ref + 同一个装载器 `shared/approval-progress`），不再单独造 `reverseProgress`。
 * 所以 ③ / ③-补 按 `approvalProgress` / `voucherApprovalInstanceId` / `EMPTY_APPROVAL_PROGRESS`
 * 断言；「送审后立刻拉进度 / 撤回后清空」这两条**行为**断言逐入口保留，
 * 也不许为了过断言把 `hasApprovalProgress` 这类判据放宽成恒真（③ 断言的是判据本身）。
 *
 * # 为什么这么测
 *
 * 这些改动全是「模板里少传一个 prop / 少挂一块」的性质：`vue-tsc` 抓不到
 * （`tasks` 是可选 prop，不传也能编译），e2e 只看得见一张有数据的单。
 * 所以两类断言：
 * 1. **源码即契约**（本仓既有姿势）：模板里必须出现 `:tasks=...`、必须不再有自绘时间线；
 * 2. **纯逻辑**：共享装载器 `shared/approval-progress` 的映射口径（+ 调拨的「提交人」一行）。
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

import type { FlowNodeProgress, FlowTaskItem } from '@/api/business/flow-api';
import {
  EMPTY_APPROVAL_PROGRESS,
  approvalProgressOf,
  loadApprovalProgress,
} from '@/views/business/erp/shared/approval-progress';
import { buildApprovalSteps, formatTime } from '@/views/business/erp/transfer/transfer-logic';
import { voucherApprovalInstanceId } from '@/views/business/erp/voucher/voucher-audit-logic';
import { groupProgressTasks, progressTaskLabel } from '@/views/business/flow/components/flow-progress-tasks';

const ROOT = resolve(process.cwd());

/** 读一页 SFC 源码（断言只做子串 / 正则，不做 AST —— 与既有源码契约测同风格） */
function sourceOf(relative: string): string {
  return readFileSync(join(ROOT, relative), 'utf8');
}

function count(text: string, needle: string): number {
  return text.split(needle).length - 1;
}

/**
 * 标记是否落在**详情抽屉**里（`<t-drawer` … `</t-drawer>`）。
 *
 * 页面里可能有好几个抽屉 / 弹窗，所以取「标记之前最近的那个 `<t-drawer`」再找它的闭合，
 * 而不是盲取第一个 `<t-drawer>`。
 */
function assertInDrawer(source: string, marker: string): number {
  const at = source.indexOf(marker);
  assert.notEqual(at, -1, `找不到 ${marker}`);
  const open = source.lastIndexOf('<t-drawer', at);
  assert.notEqual(open, -1, `${marker} 不在任何 <t-drawer> 里`);
  const close = source.indexOf('</t-drawer>', at);
  assert.notEqual(close, -1, `${marker} 之后的 </t-drawer> 丢了`);
  return at;
}

/** 取详情抽屉模板（以 `marker` 所在抽屉为准） */
function drawerOf(source: string, marker: string): string {
  const at = assertInDrawer(source, marker);
  return source.slice(source.lastIndexOf('<t-drawer', at), source.indexOf('</t-drawer>', at));
}

/**
 * 取一个**顶层** `async function` 的函数体（到下一个顶层声明为止）。
 *
 * 为什么要按函数切：只在整个文件里 `count('loadVoucherApprovalProgress(') === 4`
 * 会被「定义改了名 / 多调一次」之外的写法糊弄过去，看不出**哪一处**入口漏了刷新。
 * 顶层声明的缩进都是 0，所以用行首 `\nconst ` / `\nfunction ` 这类边界切是安全的。
 */
function fnSource(source: string, name: string): string {
  const at = source.indexOf(`async function ${name}(`);
  assert.notEqual(at, -1, `找不到 async function ${name}(`);
  const rest = source.slice(at + 1);
  const boundary = rest.search(/\n(?:async function |function |const |let |\/\/ ={6,})/);
  return boundary === -1 ? rest : rest.slice(0, boundary);
}

const STOCKIN = sourceOf('src/views/business/erp/stockin/index.vue');
const PAYABLE = sourceOf('src/views/business/erp/payable/index.vue');
const VOUCHER = sourceOf('src/views/business/erp/voucher/index.vue');
const TRANSFER = sourceOf('src/views/business/erp/transfer/index.vue');
const PURCHASE = sourceOf('src/views/business/erp/purchase/index.vue');
const FLOW_PROGRESS = sourceOf('src/views/business/flow/components/FlowProgress.vue');

// ==================== fixture（GET /business/flow/instance/{id} 的真实形状） ====================

const NODES: FlowNodeProgress[] = [
  {
    node_key: 's',
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
    activated_at: '2026-11-03T01:10:00Z',
    ended_at: '2026-11-03T01:10:00Z',
    duration_ms: 0,
  },
  {
    node_key: 'n_fin',
    node_name: '财务经理复核',
    node_type: 'APPROVAL',
    round: 1,
    status: 'ACTIVE',
    approval_mode: 'SINGLE',
    approve_ratio: 0,
    approver_count: 1,
    approved_count: 0,
    rejected_count: 0,
    is_auto_passed: false,
    taken_edge_key: '',
    activated_at: '2026-11-03T01:10:05Z',
    ended_at: null,
    duration_ms: null,
  },
];

function taskOf(over: Partial<FlowTaskItem>): FlowTaskItem {
  return {
    id: 'task-x',
    instance_id: 'inst-c2',
    instance_no: 'FLW-20261103-000001',
    title: '调拨单 TR-20261103-000001',
    flow_name: '调拨审批',
    initiator_name: '超级管理员',
    current_node_name: '财务经理复核',
    arrived_at: '2026-11-03T01:10:05Z',
    node_key: 'n_fin',
    round: 1,
    task_kind: 'APPROVAL',
    assignee_id: 'u-fin',
    assignee_name: '财务经理',
    assignee_source: 'ROLE',
    status: 'PENDING',
    instance_status: 'RUNNING',
    is_added_sign: false,
    form_data: {},
    is_self_approval: false,
    is_auto: false,
    comment: '',
    transferred_from: '',
    created_at: '2026-11-03T01:10:05Z',
    completed_at: null,
    ...over,
  };
}

const TIMELINE: FlowTaskItem[] = [
  taskOf({ id: 'task-fin', assignee_name: '财务经理', status: 'APPROVED', comment: '同意调拨', completed_at: '2026-11-03T01:12:00Z' }),
  taskOf({ id: 'task-admin', assignee_id: 'u-admin', assignee_name: '超级管理员', status: 'SKIPPED', comment: '内控：制单人不可自审', completed_at: '2026-11-03T01:12:01Z' }),
  taskOf({ id: 'task-wm', assignee_id: 'u-wm', assignee_name: '仓管员', status: 'PENDING' }),
];

// ==================== ① 收货单：模板必须传 tasks ====================

test('① 收货单详情：FlowProgress 同时传 nodes 与 tasks（否则只有进度条，看不到审批人）', () => {
  const drawer = drawerOf(STOCKIN, 'data-testid="stockin-cancel-progress"');

  assert.match(drawer, /data-testid="stockin-cancel-progress"/, '收货单详情的进度块丢了');
  assert.match(
    drawer,
    /:nodes="approvalProgress\.nodes"/,
    '收货单进度块必须传 nodes',
  );
  assert.match(
    drawer,
    /:tasks="approvalProgress\.tasks"/,
    '收货单进度块必须传 tasks（审批人 / 时间 / 意见都在 timeline 里）',
  );
  // 判据也换成「有节点才渲染」，与新数据结构一致
  assert.match(drawer, /v-if="hasApprovalProgress\(approvalProgress\)"/);
  // 老的单节点数组必须彻底退场（留着就是两套状态，迟早不一致）
  assert.equal(count(STOCKIN, 'approvalNodes'), 0, '收货单不该再有 approvalNodes（只装节点不装明细）');
});

// ==================== ② 应付单：模板必须传 tasks ====================

test('② 应付单详情：FlowProgress 同时传 nodes 与 tasks，且标题仍跟着实例类型走', () => {
  const drawer = drawerOf(PAYABLE, 'data-testid="payable-approval-progress"');

  assert.match(drawer, /data-testid="payable-approval-progress"/);
  assert.match(drawer, /:nodes="approvalProgress\.nodes"/);
  assert.match(drawer, /:tasks="approvalProgress\.tasks"/, '应付单也必须传 tasks');
  assert.match(drawer, /\{\{ approvalTitle \}\}/, '付款审批 / 作废审批两个标题不能写死');
  assert.equal(count(PAYABLE, 'approvalNodes'), 0, '应付单不该再有 approvalNodes');
});

// ==================== ③ 凭证：新增审批进度区块 ====================

test('③ 凭证详情：底部新增「审批进度」区块（节点 + 审批人明细），位置在分录 / 合计之后', () => {
  const drawer = drawerOf(VOUCHER, 'data-testid="voucher-approval-progress"');

  const entries = drawer.indexOf('data-testid="voucher-entry-table"');
  const balance = drawer.indexOf('data-testid="voucher-balance"');
  const progress = drawer.indexOf('data-testid="voucher-approval-progress"');
  assert.notEqual(progress, -1, '凭证详情没有审批进度区块');

  assert.ok(entries !== -1 && balance !== -1, '分录表 / 合计行不能丢');
  assert.ok(balance < progress, `审批进度必须在合计行之后（balance=${balance}, progress=${progress}）`);
  assert.ok(entries < progress, '审批进度必须在分录表之后');

  // 标题用共享常量（与采购单同一个词），不是页面自己写的字面量
  assert.match(drawer, /\{\{ APPROVAL_PROGRESS_TITLE \}\}/);
  // 口径：与收货 / 应付 / 调拨共用同一个 `approvalProgress` ref（P1-11 统一），
  // 不再单独造一套 `reverseProgress` —— 否则「一个页面能看到审批人、另一个看不到」的漂移会重演。
  assert.match(drawer, /:nodes="approvalProgress\.nodes"/, '凭证进度块必须传 nodes');
  assert.match(drawer, /:tasks="approvalProgress\.tasks"/, '凭证的进度块也必须传 tasks（谁 / 时间 / 意见都在 timeline 里）');
  assert.match(drawer, /v-if="hasApprovalProgress\(approvalProgress\)"/, '没有红冲实例时不该画空区块');
  assert.equal(
    count(VOUCHER, 'reverseProgress'),
    0,
    '凭证页不该再有独立的 reverseProgress（已并入共享的 approvalProgress）',
  );
});

test('③-补 凭证：进度数据源优先 `pending_reverse_instance_id`（P-FLOW-3B 下发的字段），不是自造 id', () => {
  // 装载入口读的字段：实例 id 一律经 `voucherApprovalInstanceId` 归一，页面不自己挑字段
  assert.match(
    VOUCHER,
    /loadApprovalProgress\(voucherApprovalInstanceId\(row\), getFlowInstance\)/,
    '凭证进度必须经 voucherApprovalInstanceId(row) 取实例（红冲审批中的凭证才有）',
  );
  assert.match(
    VOUCHER,
    /voucherApprovalInstanceId,/,
    'voucherApprovalInstanceId 必须从 voucher-audit-logic 引入（判据矩阵单测在 p0-fix-4b）',
  );

  // 归一判据按**行为**验（源码字符串看不出「谁优先」，四种组合都断）
  assert.equal(
    voucherApprovalInstanceId({ pending_reverse_instance_id: 'inst-rev', approval_instance_id: 'inst-audit' }),
    'inst-rev',
    '红冲审批中时实例必须取 pending_reverse_instance_id（P-FLOW-3B 下发）',
  );
  assert.equal(
    voucherApprovalInstanceId({ pending_reverse_instance_id: null, approval_instance_id: 'inst-audit' }),
    'inst-audit',
    '没有红冲审批时回落到审核链实例（4A 若挂）',
  );
  assert.equal(voucherApprovalInstanceId({ pending_reverse_instance_id: '  ', approval_instance_id: '' }), null, '空白 id 当没有');
  assert.equal(voucherApprovalInstanceId({}), null, '两个字段都没有 → 不拉进度');
  assert.equal(voucherApprovalInstanceId(null), null, '空凭证不许抛');

  // 三处入口各自都要刷新：打开详情 / 详情内跳另一张 / 送审成功
  // （换成逐入口断言，而不是全文件 count —— 少一处就会出现「送审后进度区还是空的」）
  for (const entry of ['openDetail', 'openById', 'submitReverse']) {
    assert.match(
      fnSource(VOUCHER, entry),
      /loadVoucherApprovalProgress\(detail\.value\)/,
      `${entry} 里没有重拉审批进度（少这一处就会出现「送审后进度区还是空的」）`,
    );
  }

  // 撤回后实例没了 → 进度必须清空（不许留着上一张审批单的节点）
  const withdraw = VOUCHER.slice(VOUCHER.indexOf('async function doWithdrawReverse'));
  assert.match(
    withdraw.slice(0, 800),
    /approvalProgress\.value = \{ \.\.\.EMPTY_APPROVAL_PROGRESS \}/,
    '撤回红冲审批后进度要清空',
  );
  // 反向断言：装载函数也不再叫 loadReverseProgress（旧名会让「清空」写成另一个 ref）
  assert.equal(
    (VOUCHER.match(/reverseProgress/gi) ?? []).length,
    0,
    '凭证页不该再有 reverseProgress / loadReverseProgress（已统一为 approvalProgress）',
  );
});

// ==================== ④ 调拨单：自绘时间线退场 ====================

test('④ 调拨单详情：自绘时间线整段删掉，换成全站 FlowProgress（含 tasks）', () => {
  const drawer = drawerOf(TRANSFER, 'data-testid="transfer-approval-progress"');

  assert.equal(count(TRANSFER, 'timeline__item'), 0, '自绘时间线的模板 / 样式必须删干净');
  assert.equal(count(TRANSFER, 'timeline__dot'), 0, '自绘时间线的圆点样式必须删干净');
  assert.equal(count(TRANSFER, 'transfer-approval-timeline'), 0, '老 testid 不能留着当幌子');

  assert.match(drawer, /data-testid="transfer-approval-progress"/, '调拨详情必须有 FlowProgress');
  assert.match(drawer, /:nodes="approvalProgress\.nodes"/);
  assert.match(drawer, /:tasks="approvalProgress\.tasks"/, '调拨单也要传 tasks（谁 / 时间 / 意见）');
  // 空态 / 加载 / 失败三个分支仍在（弱引用单据：草稿单没有实例）
  assert.match(drawer, /data-testid="transfer-approval-none"/);
  assert.match(drawer, /v-else-if="approvalLoading"/);
  assert.match(drawer, /v-else-if="approvalError"/);
});

test('④-补 调拨单：自绘时间线独有的「提交人 + 提交时间」用一行文字补回（不许默默丢）', () => {
  const drawer = drawerOf(TRANSFER, 'data-testid="transfer-approval-progress"');

  assert.match(drawer, /data-testid="transfer-approval-submit"/, '提交人 / 提交时间那一行丢了');
  assert.match(
    TRANSFER,
    /提交人 \$\{submit\.who\} · \$\{formatTime\(submit\.at\)\} · \$\{instance\.instance_no\}/,
    '提交行的文案口径（提交人 · 时间 · 单号）',
  );

  // 口径来自审批引擎实例（不是前端猜的）：buildApprovalSteps 的第一步就是「提交」
  const steps = buildApprovalSteps({
    instance: {
      initiator_name: '超级管理员',
      instance_no: 'FLW-20261103-000001',
      status: 'RUNNING',
      started_at: '2026-11-03T01:10:00Z',
      ended_at: null,
    },
    timeline: [],
  });
  assert.equal(steps.length, 2, '提交 + 「审批中」两步（还没结束时不出「完成」）');
  assert.deepEqual(
    [steps[0].who, steps[0].what, steps[0].at],
    ['超级管理员', '提交', '2026-11-03T01:10:00Z'],
  );
  assert.ok(formatTime(steps[0].at) !== '—', '提交时间必须能格式化（空时间会显示破折号）');
});

// ==================== ⑤ 共享装载器（抽公共逻辑的验收） ====================

test('⑤ 四个页面都从同一个共享装载器取进度（不再各写一份 map）', () => {
  for (const [name, source] of [
    ['采购', PURCHASE],
    ['收货', STOCKIN],
    ['应付', PAYABLE],
    ['调拨', TRANSFER],
    ['凭证', VOUCHER],
  ] as Array<[string, string]>) {
    assert.match(
      source,
      /from '@\/views\/business\/erp\/shared\/approval-progress'/,
      `${name}页没有用共享的 approval-progress（会出现「一个页面能看到审批人、另一个看不到」）`,
    );
  }
  // 共享模块里必须真的有 tasks 映射（不然传了 `:tasks` 也是空数组）
  const shared = sourceOf('src/views/business/erp/shared/approval-progress.ts');
  assert.match(shared, /export function mapApprovalTasks/);
  assert.match(shared, /export function approvalProgressOf/);
});

test('⑤-补 approvalProgressOf：实例详情一次映射成 nodes + tasks，null 退化成空（不抛）', () => {
  const progress = approvalProgressOf({ nodes: NODES, timeline: TIMELINE });

  assert.deepEqual(
    progress.nodes.map((node) => [node.name, node.status]),
    [
      ['开始', 'APPROVED'],
      ['财务经理复核', 'ACTIVE'],
    ],
  );
  // 三个审批人都要带出来（含 SKIPPED 的留痕）
  assert.deepEqual(
    progress.tasks.map((task) => [task.assignee_name, task.status]),
    [
      ['财务经理', 'APPROVED'],
      ['超级管理员', 'SKIPPED'],
      ['仓管员', 'PENDING'],
    ],
  );
  // 跳过说明（内控原因）不许被过滤掉 —— 这是「为什么没人审」的唯一解释
  const groups = groupProgressTasks(progress.nodes, progress.tasks);
  assert.equal(groups.length, 1, '只有「财务经理复核」有审批人，其余节点不出空组');
  assert.equal(groups[0].name, '财务经理复核');
  assert.deepEqual(
    groups[0].tasks.map((task) => progressTaskLabel(task.status)),
    ['已同意', '已跳过', '待处理'],
  );
  assert.equal(groups[0].tasks[1].comment, '内控：制单人不可自审');

  // 缺字段 / null → 空结果（页面判据 hasApprovalProgress 直接为 false）
  assert.deepEqual(approvalProgressOf(null), EMPTY_APPROVAL_PROGRESS);
  assert.deepEqual(approvalProgressOf({}), EMPTY_APPROVAL_PROGRESS);
  assert.deepEqual(approvalProgressOf({ nodes: null, timeline: null }), EMPTY_APPROVAL_PROGRESS);
});

test('⑤-补 loadApprovalProgress 与 approvalProgressOf 同口径（凭证 / 收货 / 应付都走它）', async () => {
  const calls: string[] = [];
  const instance = { nodes: NODES, timeline: TIMELINE };
  const progress = await loadApprovalProgress('inst-c2', async (id) => {
    calls.push(id);
    return instance;
  });

  assert.deepEqual(calls, ['inst-c2'], '有实例 id 就拉一次');
  assert.deepEqual(progress, approvalProgressOf(instance), '两条路径必须给出同一结果');

  // 没有实例 id → 一次都不拉（凭证没红冲审批时不能白打接口）
  const none = await loadApprovalProgress(null, async (id) => {
    calls.push(id);
    return instance;
  });
  assert.deepEqual(calls, ['inst-c2'], '没有实例 id 就不该发请求');
  assert.deepEqual(none, EMPTY_APPROVAL_PROGRESS);
});

// ==================== ⑥ 组件契约不变（四个页面共用的组件不许动） ====================

test('⑥ FlowProgress 组件契约未变：nodes 必填、tasks 可选（不传的老页面像素不变）', () => {
  assert.match(FLOW_PROGRESS, /nodes: FlowProgressNode\[\];/);
  assert.match(FLOW_PROGRESS, /tasks\?: FlowProgressTask\[\];/);

  // 明细区仍由 `tasks` 决定出不出（不传 → 整块不渲染）
  assert.match(FLOW_PROGRESS, /v-if="taskGroups\.length > 0"/);
  // 进度条本身仍然是同一份渲染代码（没有为某个单据加分支）
  for (const doc of ['stockin', 'payable', 'voucher', 'transfer', 'purchase']) {
    assert.ok(
      !FLOW_PROGRESS.includes(doc),
      `组件里不该出现「${doc}」这种按单据分支的写法（四个页面共用同一份渲染）`,
    );
  }
});

/**
 * Vue Flow 画布「业务贴合」单测（VUE-FLOW-2）。
 *
 * 运行：`pnpm test:unit`。
 *
 * 卡面 ①–⑪ 里一半是**视觉/交互契约**，vue-tsc 与几何断言都看不见：
 * - 校验角标必须画在卡片**外面**（卡片 overflow:hidden 会把它裁成半圆）；
 * - 只读态**不能**再用 `v-if` 把 Handle 摘掉（Vue Flow 靠它算锚点，摘了 → 连线变成底部出/顶部进，
 *   这是用户真机截图反馈过的 bug）；
 * - 只读态底部提示不能再写「从右侧圆点拖出连线」（这时没有圆点可拖）；
 * - 走过边的 key 口径是 `${from}>${to}`。
 *
 * 所以这里一部分用**纯函数**断言（`buildNodeView` / `isEdgeTaken`），
 * 一部分用 `readFileSync` 读 SFC 做**源码级守门**（写错就红，不依赖真机）。
 *
 * 视觉一致性的真机证据在 `scripts/verify/flow_vf2_business_verify.cjs`
 * （74 条断言 + 并排像素差），本文只钉「一改就回归」的那几条。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  NODE_GLYPH,
  STATUS_MARK,
  STATUS_TEXT,
  STATUS_TONE,
  buildNodeView,
  isEdgeTaken,
  nodeSizeOf,
} from '../../src/views/business/flow/components/flow-node-view.ts';
import type { FlowNode } from '../../src/api/business/flow-api.ts';

const HERE = dirname(fileURLToPath(import.meta.url));
const DIR = resolve(HERE, '../../src/views/business/flow/components');
const NEW_SRC = readFileSync(resolve(DIR, 'FlowCanvasVueFlow.vue'), 'utf-8');
const OLD_SRC = readFileSync(resolve(DIR, 'FlowCanvas.vue'), 'utf-8');
const EDIT_SRC = readFileSync(resolve(HERE, '../../src/views/business/flow/def/edit.vue'), 'utf-8');

/** 去掉注释再断言（注释里出现同样的字符串会骗过 includes） */
function stripComments(src: string): string {
  return src
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .split('\n')
    .map((line) => line.replace(/\/\/.*$/, ''))
    .join('\n');
}

const NEW_CODE = stripComments(NEW_SRC);
const OLD_CODE = stripComments(OLD_SRC);

/** 造节点 */
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
    timeout_action: 'REMIND',
    remind_before_hours: null,
    max_remind_times: 0,
    cc_on_approved: false,
    config: null,
    assignees: [],
    ...patch,
  } as FlowNode;
}

// ==================== ① 校验角标 ====================

test('① 校验角标：issueKeys 命中 → has-issue（红虚线框 + 角标）；未命中不带', () => {
  const hit = buildNodeView(node({ key: 'a' }), { issueKeys: new Set(['a']) });
  const miss = buildNodeView(node({ key: 'b' }), { issueKeys: new Set(['a']) });
  assert.equal(hit.cls['has-issue'], true);
  assert.equal(miss.cls['has-issue'], false);
});

test('① 校验角标画在卡片**外面**（卡片 overflow:hidden，放里面会被裁成半圆）+ 带悬停原因', () => {
  // 角标必须在 </div> 之后、Handle 之前：用「卡片闭合 → 角标」的顺序断言
  const cardClose = NEW_CODE.indexOf('</div>\n\n        <!-- 校验角标');
  assert.ok(cardClose > 0, '角标没有紧跟卡片闭合标签（可能又被挪回卡片里了）');
  assert.match(NEW_CODE, /class="flow-node__issue"[^>]*:title="ISSUE_HINT"/, '角标缺悬停原因（title）');
  assert.match(NEW_CODE, /const ISSUE_HINT = '/, '缺 ISSUE_HINT 常量');
  // 几何：16×16 + 右上角外扩 4px（= 老画布 circle(w-4, 4, r=8)）
  const issueBlock = NEW_SRC.slice(NEW_SRC.indexOf('&__issue {'), NEW_SRC.indexOf('&__issue {') + 500);
  assert.match(issueBlock, /width: 16px/, `角标宽度必须 16px：${issueBlock}`);
  assert.match(issueBlock, /height: 16px/, `角标高度必须 16px：${issueBlock}`);
  assert.match(issueBlock, /top: -4px/, `角标要外扩 4px：${issueBlock}`);
  assert.match(issueBlock, /right: -4px/, `角标要外扩 4px：${issueBlock}`);
});

// ==================== ② 节点运行状态 ====================

test('② 5 种状态：角标内容 / 文案 / 底色档位与老画布逐字一致', () => {
  assert.deepEqual(
    Object.keys(STATUS_TONE).sort(),
    ['ACTIVE', 'APPROVED', 'PENDING', 'REJECTED', 'SKIPPED', 'WAITING'],
    '状态表少了某种状态',
  );
  assert.deepEqual(STATUS_MARK, { APPROVED: '✓', ACTIVE: '', REJECTED: '✕', SKIPPED: '', WAITING: '', PENDING: '' });
  assert.deepEqual(STATUS_TEXT, { APPROVED: '已通过', ACTIVE: '处理中', REJECTED: '已驳回', SKIPPED: '已跳过', WAITING: '待处理', PENDING: '待处理' });
  // 档位 → class：approved / active / rejected / pending（pending 同时覆盖 SKIPPED / WAITING）
  assert.deepEqual(
    ['APPROVED', 'ACTIVE', 'REJECTED', 'SKIPPED', 'WAITING'].map((key) => STATUS_TONE[key]),
    ['approved', 'active', 'rejected', 'pending', 'pending'],
  );
});

test('② 状态色 class + ACTIVE 脉冲动画都还在（脉冲是「当前走到哪」的关键视觉）', () => {
  const active = buildNodeView(node({ key: 'a' }), { status: { a: 'ACTIVE' } });
  const approved = buildNodeView(node({ key: 'a' }), { status: { a: 'APPROVED' } });
  const rejected = buildNodeView(node({ key: 'a' }), { status: { a: 'REJECTED' } });
  const skipped = buildNodeView(node({ key: 'a' }), { status: { a: 'SKIPPED' } });
  assert.equal(active.cls['is-active'], true);
  assert.equal(approved.cls['is-approved'], true);
  assert.equal(rejected.cls['is-rejected'], true);
  assert.equal(skipped.cls['is-skipped'], true);
  assert.equal(approved.statusMark, '✓');
  assert.equal(rejected.statusMark, '✕');
  // 脉冲：ACTIVE 的状态点 ::after 跑 flow-status-pulse
  assert.match(NEW_SRC, /&--active \{[\s\S]{0,300}animation: flow-status-pulse/, 'ACTIVE 状态点丢了脉冲动画');
  assert.match(NEW_SRC, /@keyframes flow-status-pulse/, '缺 flow-status-pulse 关键帧');
});

// ==================== ③ 锚点（含「Handle 不能摘」的 bug 回归） ====================

test('③ 只读态**仍然渲染** Handle（v-if 摘掉会让 Vue Flow 锚点退化成 bottom/top）', () => {
  const tags = (NEW_CODE.match(/<Handle\b[^>]*>/g) ?? []).map((tag) => tag.replace(/\s+/g, ' '));
  assert.equal(tags.length, 2, `模板里应有 2 个 Handle，实际 ${tags.length}`);
  for (const tag of tags) {
    assert.doesNotMatch(tag, /v-if="!readonly"/, `Handle 又用 v-if="!readonly" 摘掉了（真机 bug 会复现）：${tag}`);
    assert.match(tag, /:connectable="!readonly"/, `Handle 应该用 connectable 控制而不是不渲染：${tag}`);
  }
  // 只读态视觉隐藏用 opacity，不能 display:none / visibility:hidden / width:0（都会让锚点退化）
  assert.match(NEW_SRC, /\.is-readonly\) \{[\s\S]{0,120}opacity: 0;/, '只读态应该用 opacity: 0 隐藏句柄');
  assert.doesNotMatch(NEW_SRC, /display: none;[\s\S]{0,80}flow-node__port/, '句柄不能用 display:none 隐藏');
});

test('③ 节点上声明 sourcePosition/targetPosition 兜底（Handle 缺失也不会退化成上下）', () => {
  assert.match(NEW_CODE, /sourcePosition: Position\.Right/, '节点缺 sourcePosition: Right 兜底');
  assert.match(NEW_CODE, /targetPosition: Position\.Left/, '节点缺 targetPosition: Left 兜底');
});

test('③ 锚点圆点用内嵌 SVG circle（r=5 / 描边 1.5），与老画布同一条几何', () => {
  const circles = NEW_CODE.match(/<circle class="flow-node__port-dot"[^>]*>/g) ?? [];
  assert.equal(circles.length, 2, `应有 2 个锚点圆，实际 ${circles.length}`);
  for (const circle of circles) {
    assert.match(circle, /r="5"/, `锚点半径必须 5：${circle}`);
  }
  assert.match(NEW_SRC, /&__port-dot \{[\s\S]{0,200}stroke-width: 1\.5/, '锚点描边必须 1.5px');
  // 老画布是 circle r=5 + stroke-width 1.5，口径必须一致
  assert.match(OLD_CODE, /class="flow-node__port" :cx="node\.x"/, '老画布锚点结构变了');
});

// ==================== ④ 已走过的边 ====================

test('④ takenPairs 口径是 `${from}>${to}`（写错分隔符 → 进度图上整片不亮）', () => {
  const pairs = ['s>c_amt', 'c_amt>n_dept'];
  assert.equal(isEdgeTaken(pairs, 's', 'c_amt'), true);
  assert.equal(isEdgeTaken(pairs, 'c_amt', 'n_dept'), true);
  assert.equal(isEdgeTaken(pairs, 'c_amt', 's'), false, '方向反了不该命中');
  assert.equal(isEdgeTaken(pairs, 's', 'n_dept'), false);
  assert.equal(isEdgeTaken([], 's', 'c_amt'), false);
  assert.equal(isEdgeTaken(undefined, 's', 'c_amt'), false);
  // 用 Set 也要能过（props 传数组，内部可能先转 Set）
  assert.equal(isEdgeTaken(new Set(pairs), 's', 'c_amt'), true);
  // 画布确实用这个函数 + 走的边换箭头 marker
  assert.match(NEW_CODE, /isEdgeTaken\(takenSet\.value, edge\.from, edge\.to\)/, '画布没走 isEdgeTaken');
  assert.match(NEW_CODE, /'is-taken': taken/, '边视图缺 is-taken class');
  assert.match(NEW_CODE, /url\(#vf-flow-arrow-taken\)/, '走的边没换箭头 marker');
  assert.match(NEW_SRC, /&\.is-taken &__line \{[\s\S]{0,120}stroke-width: 2\.4/, '走过的边加粗值变了（老画布 2.4px）');
});

// ==================== ⑤ 选中态 / ⑥ 只读态 / ⑨ 键盘 ====================

test('⑤ 选中态：class 由 props 驱动（不依赖 Vue Flow 内部 selection）', () => {
  const selected = buildNodeView(node({ key: 'a' }), { selectedKey: 'a' });
  const other = buildNodeView(node({ key: 'b' }), { selectedKey: 'a' });
  assert.equal(selected.cls['is-selected'], true);
  assert.equal(other.cls['is-selected'], false);
  // elements-selectable 关闭 + 三个点击事件自己 emit
  assert.match(NEW_CODE, /:elements-selectable="false"/, '不能用 Vue Flow 内部 selection（像素对不齐）');
  assert.match(NEW_CODE, /@node-click="onNodeClick"/, '缺 node-click');
  assert.match(NEW_CODE, /@edge-click="onEdgeClick"/, '缺 edge-click');
  assert.match(NEW_CODE, /@pane-click="onPaneClick"/, '缺 pane-click');
  // 点空白 = select-node(null) + select-edge(null)（老画布同一出口，没有 clear-selection 事件）
  assert.match(NEW_CODE, /emit\('select-node', null\)/, '点空白没清节点选中');
  assert.match(NEW_CODE, /emit\('select-edge', null\)/, '点空白没清边选中');
});

test('⑥ 只读态：不可拖 / 不可连 / Delete 无效（三个开关 + 键盘判断）', () => {
  assert.match(NEW_CODE, /:nodes-draggable="!readonly && !spaceDown"/, '只读态应禁用拖拽');
  assert.match(NEW_CODE, /:nodes-connectable="!readonly"/, '只读态应禁用连线');
  assert.match(NEW_CODE, /:delete-key-code="null"/, 'Delete 必须交给自己的键盘逻辑（否则双重删除）');
  assert.match(NEW_CODE, /\(event\.key === 'Delete' \|\| event\.key === 'Backspace'\) && !props\.readonly/, 'Delete 没判只读');
  assert.match(NEW_CODE, /draggable: !props\.readonly/, '节点数据上的 draggable 没跟只读走');
  assert.match(NEW_CODE, /connectable: !props\.readonly/, '节点数据上的 connectable 没跟只读走');
});

test('⑨ 键盘：Delete 删除、空格平移、Esc 取消（Esc 是本卡新增，老画布没有）', () => {
  assert.match(NEW_CODE, /if \(event\.code === 'Space'\) \{[\s\S]{0,120}spaceDown\.value = true/, '缺空格平移');
  assert.match(NEW_CODE, /if \(event\.key === 'Escape'\) \{/, '缺 Esc 处理');
  assert.match(NEW_CODE, /connectCancelled\.value = true/, 'Esc 没有放弃正在拉的线');
  assert.match(NEW_CODE, /emit\('delete-selected'\)/, '缺 Delete → delete-selected');
  // Esc 取消选中走与「点空白」同一个出口
  const escBlock = NEW_CODE.slice(NEW_CODE.indexOf("if (event.key === 'Escape')"), NEW_CODE.indexOf("if (event.key === 'Escape')") + 700);
  assert.match(escBlock, /emit\('select-node', null\)/, `Esc 没清节点选中：${escBlock.slice(0, 200)}`);
  assert.match(escBlock, /emit\('select-edge', null\)/, `Esc 没清边选中：${escBlock.slice(0, 200)}`);
});

// ==================== ⑦ 拖拽创建（调用方驱动，落点靠 screenToCanvas） ====================

test('⑦ 左侧面板拖拽创建：调用方用 screenToCanvas 换算落点（两套画布同一入口）', () => {
  // 画布暴露 screenToCanvas（Vue Flow 用 screenToFlowCoordinate，已扣掉 translate/scale）
  assert.match(NEW_CODE, /function screenToCanvas\(clientX: number, clientY: number\): Point \{[\s\S]{0,200}screenToFlowCoordinate/, '新画布 screenToCanvas 没走 screenToFlowCoordinate');
  assert.match(NEW_CODE, /screenToCanvas,/, 'defineExpose 里没暴露 screenToCanvas');
  // 调用方（def/edit.vue）在松手时用它算坐标并建节点
  assert.match(EDIT_SRC, /canvasRef\.value\?\.screenToCanvas\(event\.clientX, event\.clientY\)/, 'edit.vue 没用自己的画布换算落点');
  assert.match(EDIT_SRC, /makeNode\(drag\.type, point,/, 'edit.vue 建节点没按落点');
  // 拖入的预览轮廓（幽灵）
  assert.match(EDIT_SRC, /flow-editor__ghost/, '缺拖入预览（幽灵）');
});

// ==================== ⑩ 底部提示 ====================

test('⑩ 底部提示：编辑态文案逐字一致；只读态改成只读说明（不再提示「拖出连线」）', () => {
  const editing = '拖节点移动 · 从右侧圆点拖出连线 · 滚轮缩放 · 空格/空白处拖拽平移 · Delete 删除';
  const readonly = '只读模式（已发布版本）· 如需修改请点顶部「另存为新草稿」';
  for (const [label, src] of [['新画布', NEW_CODE], ['老画布', OLD_CODE]] as const) {
    assert.ok(src.includes(editing), `${label} 的编辑态提示文案变了`);
    assert.ok(src.includes(readonly), `${label} 的只读态提示没改成只读说明`);
    assert.match(src, /\{\{\s*readonly\s*\?\s*'只读模式/, `${label} 的提示没按 readonly 分支`);
  }
});

// ==================== ⑪ 副标题 ====================

test('⑪ 副标题 11 种情况：会签 3 / 缺配置 2 / 类型说明 4 / 运行状态 2', () => {
  const cases: Array<[string, FlowNode, Record<string, string>, string]> = [
    ['会签-全部同意', node({ type: 'COUNTERSIGN', approval_mode: 'ALL' }), {}, '全部同意'],
    ['会签-或签', node({ type: 'COUNTERSIGN', approval_mode: 'ANY' }), {}, '或签'],
    ['会签-按比例', node({ type: 'COUNTERSIGN', approval_mode: 'RATIO', approve_ratio: 0.5 }), {}, '按比例 50%'],
    ['审批-缺审批人', node({ type: 'APPROVAL', assignee_type: null }), {}, '未配置审批人'],
    ['任务-缺负责人', node({ type: 'TASK_NODE', assignee_type: null }), {}, '未配置负责人'],
    ['任务-默认生成任务', node({ type: 'TASK_NODE' }), {}, '生成任务'],
    ['任务-完成后审批', node({ type: 'TASK_NODE', config: { task: { on_complete: 'APPROVE' } } }), {}, '完成后审批'],
    ['通知节点', node({ type: 'NOTIFY_NODE' }), {}, '发通知（发完即通过）'],
    ['条件网关', node({ type: 'CONDITION' }), {}, '分支网关'],
    ['并行-或分叉', node({ type: 'PARALLEL', fork_mode: 'OR' }), {}, '或分叉'],
    ['抄送-只读知会', node({ type: 'CC' }), {}, '只读知会'],
    ['开始', node({ type: 'START' }), {}, '入口'],
    ['结束', node({ type: 'END' }), {}, '出口'],
    ['状态-已通过', node({ key: 'x' }), { x: 'APPROVED' }, '已通过'],
    ['状态-处理中', node({ key: 'x' }), { x: 'ACTIVE' }, '处理中'],
  ];
  for (const [label, item, status, expected] of cases) {
    const view = buildNodeView(item, { status });
    assert.equal(view.subLabel, expected, `${label} 的副标题不对：${view.subLabel}`);
  }
  // 业务信息优先于运行状态：会签模式 + 状态 → 「按比例 50% · 处理中」
  const both = buildNodeView(node({ key: 'x', type: 'COUNTERSIGN', approval_mode: 'RATIO', approve_ratio: 0.5 }), { status: { x: 'ACTIVE' } });
  assert.equal(both.subLabel, '按比例 50% · 处理中');
});

test('⑪ 副标题规则没有「两份实现漂移」：老画布里的每条文案在共用实现里都存在', () => {
  // 副标题规则在 VUE-FLOW-1 就抽到了 flow-node-view.ts（画布只剩调用），
  // 老画布为了当对照基线保留内联实现 —— 文案必须两边都在，改一处漏一处就红。
  const VIEW_SRC = readFileSync(resolve(DIR, 'flow-node-view.ts'), 'utf-8');
  const literals = ['全部同意', '或签', '按比例 ', '未配置审批人', '未配置负责人', '生成任务', '发通知（发完即通过）', '分支网关', '或分叉', '只读知会', '入口', '出口'];
  for (const literal of literals) {
    assert.ok(OLD_CODE.includes(literal), `老画布缺文案「${literal}」（老画布被意外改动了？）`);
    assert.ok(VIEW_SRC.includes(literal), `共用实现（flow-node-view.ts）缺文案「${literal}」`);
  }
  // 画布必须调用共用实现，不许在组件里再复制一份 nodeSubLabel
  assert.match(NEW_CODE, /buildNodeView\(/, '新画布没调用 buildNodeView（共用实现）');
  assert.doesNotMatch(NEW_CODE, /未配置审批人/, '新画布里又内联了一份副标题文案（与 flow-node-view.ts 重复）');
});

test('⑪ 图标 / 尺寸：9 种节点一个不落，尺寸口径与老画布同一份（NODE_META）', () => {
  const types = ['START', 'APPROVAL', 'COUNTERSIGN', 'CONDITION', 'PARALLEL', 'CC', 'END', 'TASK_NODE', 'NOTIFY_NODE'];
  assert.deepEqual(Object.keys(NODE_GLYPH).sort(), [...types].sort());
  for (const type of types) {
    const size = nodeSizeOf(type);
    assert.ok(size.w > 0 && size.h > 0, `${type} 尺寸异常`);
    // 未知类型兜底成审批尺寸
    assert.deepEqual(nodeSizeOf('UNKNOWN_TYPE'), nodeSizeOf('APPROVAL'));
  }
});

// ==================== ⑧ 边标签 ====================

test('⑧ 条件标签：兜底分支显示「其他情况」、虚线；实线分支显示自身 label', () => {
  assert.match(NEW_CODE, /const label = edge\.is_default \? '其他情况' : \(edge\.label \?\? ''\)/, '标签文案口径变了');
  assert.match(NEW_CODE, /'is-default': Boolean\(data\.isDefault\)/, '缺 is-default class（虚线分支）');
  assert.match(NEW_SRC, /&\.is-default &__line \{[\s\S]{0,80}stroke-dasharray: 5 3/, '兜底分支虚线样式变了');
  // 标签底框：模板里 rx="3"（白底 + 1px 边框在样式块里）
  const labelTag = (NEW_CODE.match(/<rect\b[^>]*class=""[^>]*rx="3"[^>]*>/g) ?? NEW_CODE.match(/<rect[^>]*rx="3"[^>]*>/g) ?? []).join('\n');
  assert.match(labelTag, /rx="3"/, '标签底框丢了 3px 圆角');
  assert.match(NEW_SRC, /&__label \{[\s\S]{0,400}fill: var\(--td-bg-color-container\)/, '标签底色不是容器底色');
  // 标签位置：边中点（与老画布同一函数）
  assert.match(NEW_CODE, /edgeLabelPoint\(from, to\)/, '标签没走 edgeLabelPoint（边中点）');
});

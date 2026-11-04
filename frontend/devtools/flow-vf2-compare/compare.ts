/**
 * VUE-FLOW-2 临时比对台（跑完删除，不进提交）。
 *
 * 目的：老画布与 Vue Flow 画布**在同一页、同一份数据、同一视口（40,40,100%）**并排渲染，
 * 这样两张 `.flow-canvas` 的元素截图可以直接逐像素相减（不涉及 fitView 的取整差）。
 *
 * 场景由 `?case=` 选择：
 * - `plain`    9 种节点 / 10 条边（基线视差）
 * - `status`   5 种运行状态 + 已走过的边 + 条件标签（②④⑧）
 * - `issue`    校验角标（①）
 * - `readonly` 只读态（③⑥）
 * - `sub`      副标题 11 种情况（⑪）
 *
 * 事件全部记到 `window.__events` 与页面底部 `<pre>`，供脚本断言 emit 契约（⑤⑨⑩）。
 */
import { createApp, h, nextTick, ref } from 'vue';
import TDesignIcons from 'tdesign-icons-vue-next';
import 'tdesign-vue-next/es/style/index.css';
import '@/style/index.less';
import NativeCanvas from '@/views/business/flow/components/FlowCanvas.vue';
import VueFlowCanvas from '@/views/business/flow/components/FlowCanvasVueFlow.vue';
import type { FlowEdge, FlowNode } from '@/api/business/flow-api';

type CaseName = 'plain' | 'status' | 'issue' | 'readonly' | 'sub';

const params = new URLSearchParams(location.search);
const CASE = (params.get('case') || 'plain') as CaseName;

/** 造一个节点（只为画布渲染，字段按后端 `GraphNodeRes` 补齐，null 表示未配置） */
function node(patch: Partial<FlowNode> & Pick<FlowNode, 'key' | 'type' | 'x' | 'y'>): FlowNode {
  return {
    name: '',
    sort_order: 0,
    assignee_type: null,
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

/** 造一条边 */
function edge(key: string, from: string, to: string, label: string | null = null, isDefault = false, priority = 1): FlowEdge {
  return { key, from, to, label, is_default: isDefault, priority, condition: null } as FlowEdge;
}

/** 9 种节点 / 10 条边（与后端夹具同形） */
function plainGraph(): { nodes: FlowNode[]; edges: FlowEdge[] } {
  const nodes = [
    node({ key: 's', name: '开始', type: 'START', x: 40, y: 210 }),
    node({ key: 'task1', name: '填写申请', type: 'TASK_NODE', x: 200, y: 200, assignee_type: 'INITIATOR', config: { task: { title_template: 't', on_complete: 'DIRECT' } } }),
    node({ key: 'cond1', name: '金额分级', type: 'CONDITION', x: 420, y: 190 }),
    node({ key: 'appr1', name: '部门主管审批', type: 'APPROVAL', x: 640, y: 80, assignee_type: 'INITIATOR_SUPERIOR' }),
    node({ key: 'appr2', name: '财务复核', type: 'APPROVAL', x: 640, y: 200, assignee_type: 'ROLE' }),
    node({ key: 'cs1', name: '总经理会签', type: 'COUNTERSIGN', x: 640, y: 330, assignee_type: 'ROLE', approval_mode: 'ALL' }),
    node({ key: 'par1', name: '并行处理', type: 'PARALLEL', x: 900, y: 140 }),
    node({ key: 'cc1', name: '抄送行政', type: 'CC', x: 1120, y: 30, assignee_type: 'ROLE' }),
    node({ key: 'note1', name: '通知申请人', type: 'NOTIFY_NODE', x: 1120, y: 170, assignee_type: 'INITIATOR' }),
    node({ key: 'e', name: '结束', type: 'END', x: 1340, y: 190 }),
  ];
  const edges = [
    edge('e1', 's', 'task1'),
    edge('e2', 'task1', 'cond1'),
    edge('e3', 'cond1', 'appr1', '金额 > 1000'),
    edge('e4', 'cond1', 'appr2', '金额 ≤ 1000'),
    edge('e5', 'cond1', 'cs1', null, true),
    edge('e6', 'appr1', 'par1'),
    edge('e7', 'appr2', 'par1'),
    edge('e8', 'cs1', 'cc1'),
    edge('e9', 'par1', 'note1'),
    edge('e10', 'note1', 'e'),
  ];
  return { nodes, edges };
}

/** 5 种运行状态 + 已走过的边 + 长标签（②④⑧） */
function statusGraph(): { nodes: FlowNode[]; edges: FlowEdge[] } {
  const nodes = [
    node({ key: 's', name: '开始', type: 'START', x: 40, y: 200 }),
    node({ key: 'n_appr', name: '主管审批', type: 'APPROVAL', x: 220, y: 100, assignee_type: 'INITIATOR_SUPERIOR' }),
    node({ key: 'n_cs', name: '财务会签', type: 'COUNTERSIGN', x: 220, y: 210, assignee_type: 'ROLE', approval_mode: 'RATIO', approve_ratio: 0.5 }),
    node({ key: 'n_task', name: '生成任务', type: 'TASK_NODE', x: 220, y: 320, assignee_type: 'INITIATOR', config: { task: { title_template: 't', on_complete: 'VERIFY' } } }),
    node({ key: 'n_rej', name: '质量复核', type: 'APPROVAL', x: 480, y: 100, assignee_type: 'ROLE' }),
    node({ key: 'n_skip', name: '知会财务', type: 'CC', x: 480, y: 210, assignee_type: 'ROLE' }),
    node({ key: 'n_wait', name: '分管领导审批', type: 'APPROVAL', x: 480, y: 320, assignee_type: 'ROLE' }),
    node({ key: 'n_none', name: '无状态节点', type: 'APPROVAL', x: 480, y: 430, assignee_type: 'ROLE' }),
    node({ key: 'cond', name: '金额分级', type: 'CONDITION', x: 740, y: 200 }),
    node({ key: 'e', name: '结束', type: 'END', x: 960, y: 200 }),
  ];
  const edges = [
    edge('t1', 's', 'n_appr', '差异金额 < 10000（单级审批）'),
    edge('t2', 's', 'n_cs'),
    edge('t3', 's', 'n_task', null, true),
    edge('t4', 'n_appr', 'n_rej'),
    edge('t5', 'n_cs', 'n_skip'),
    edge('t6', 'n_task', 'n_wait'),
    edge('t7', 'n_rej', 'cond'),
    edge('t8', 'cond', 'e', '其他情况', true),
  ];
  return { nodes, edges };
}

/** 副标题 11 种情况（⑪） */
function subGraph(): { nodes: FlowNode[]; edges: FlowEdge[] } {
  const nodes = [
    node({ key: 'a1', name: '会签-全部', type: 'COUNTERSIGN', x: 60, y: 60, assignee_type: 'ROLE', approval_mode: 'ALL' }),
    node({ key: 'a2', name: '会签-或签', type: 'COUNTERSIGN', x: 300, y: 60, assignee_type: 'ROLE', approval_mode: 'ANY' }),
    node({ key: 'a3', name: '会签-比例', type: 'COUNTERSIGN', x: 540, y: 60, assignee_type: 'ROLE', approval_mode: 'RATIO', approve_ratio: 0.5 }),
    node({ key: 'b1', name: '审批-缺人', type: 'APPROVAL', x: 780, y: 60 }),
    node({ key: 'b2', name: '任务-缺人', type: 'TASK_NODE', x: 60, y: 200 }),
    node({ key: 'c1', name: '任务-验收', type: 'TASK_NODE', x: 300, y: 200, assignee_type: 'ROLE', config: { task: { title_template: 't', on_complete: 'VERIFY' } } }),
    node({ key: 'c2', name: '任务-无配置', type: 'TASK_NODE', x: 540, y: 200, assignee_type: 'ROLE' }),
    node({ key: 'c3', name: '通知节点', type: 'NOTIFY_NODE', x: 780, y: 200, assignee_type: 'ROLE' }),
    node({ key: 'c4', name: '条件网关', type: 'CONDITION', x: 60, y: 340 }),
    node({ key: 'c5', name: '并行-或', type: 'PARALLEL', x: 300, y: 340, fork_mode: 'OR' }),
    node({ key: 'c6', name: '抄送节点', type: 'CC', x: 540, y: 340, assignee_type: 'ROLE' }),
    node({ key: 'c7', name: '开始', type: 'START', x: 780, y: 340 }),
    node({ key: 'c8', name: '结束', type: 'END', x: 60, y: 460 }),
    node({ key: 'd1', name: '已通过的审批', type: 'APPROVAL', x: 300, y: 460, assignee_type: 'ROLE' }),
    node({ key: 'd2', name: '处理中的审批', type: 'APPROVAL', x: 540, y: 460, assignee_type: 'ROLE' }),
  ];
  return { nodes, edges: [edge('s1', 'a1', 'a2')] };
}

/** 各场景数据 + 画布尺寸 */
const SCENES: Record<CaseName, { w: number; h: number; build: () => { nodes: FlowNode[]; edges: FlowEdge[] } }> = {
  plain: { w: 1560, h: 520, build: plainGraph },
  status: { w: 1180, h: 700, build: statusGraph },
  issue: { w: 640, h: 420, build: plainGraph },
  readonly: { w: 1560, h: 520, build: plainGraph },
  sub: { w: 960, h: 600, build: subGraph },
};

const scene = SCENES[CASE];
const graph = scene.build();

/** 事件日志（脚本读 `window.__events`） */
const events: Array<{ at: number; canvas: string; event: string; args: unknown }> = [];
(window as unknown as { __events: typeof events }).__events = events;

function log(canvas: string, event: string, args: unknown): void {
  events.push({ at: Math.round(performance.now()), canvas, event, args });
  const out = document.getElementById('events');
  if (out) {
    out.textContent = JSON.stringify(events.slice(-24), null, 1);
  }
}

/** 两套画布共用的事件表（父组件 = 本比对台） */
function canvasProps(engine: 'native' | 'vueflow') {
  const nodes = ref(graph.nodes.map((item) => ({ ...item })));
  const edges = ref(graph.edges.map((item) => ({ ...item })));
  const selectedKey = ref<string | null>(null);
  const selectedEdgeKey = ref<string | null>(null);
  return {
    nodes,
    edges,
    selectedKey,
    selectedEdgeKey,
    props: () =>
      ({
        nodes: nodes.value,
        edges: edges.value,
        readonly: CASE === 'readonly',
        selectedKey: selectedKey.value,
        selectedEdgeKey: selectedEdgeKey.value,
        fitOnMount: false,
        issueNodeKeys: CASE === 'issue' ? ['appr1', 'cs1'] : [],
        nodeStatus:
          CASE === 'status'
            ? { n_appr: 'APPROVED', n_cs: 'ACTIVE', n_task: 'REJECTED', n_skip: 'SKIPPED', n_wait: 'WAITING' }
            : CASE === 'sub'
              ? { d1: 'APPROVED', d2: 'ACTIVE' }
              : {},
        takenPairs: CASE === 'status' ? ['s>n_appr', 'n_appr>n_rej', 'n_rej>cond'] : [],
        // `sub` 场景卡片铺满画布：关掉小地图/提示/工具栏，避免它们盖在卡片上污染逐卡片像素差
        showToolbar: CASE !== 'sub',
        showMinimap: CASE !== 'sub',
        showHint: CASE !== 'sub',
      }) as Record<string, unknown>,
    /** 事件监听（Vue 只认 `onXxx` 形式的 handler key，不能写 `on: {...}` 对象） */
    listeners: () =>
      ({
        onSelectNode: (key: string | null) => {
          selectedKey.value = key;
          if (key !== null) {
            selectedEdgeKey.value = null;
          }
          log(engine, 'select-node', key);
        },
        onSelectEdge: (key: string | null) => {
          selectedEdgeKey.value = key;
          if (key !== null) {
            selectedKey.value = null;
          }
          log(engine, 'select-edge', key);
        },
        onMoveNode: (payload: { key: string; x: number; y: number }) => {
          nodes.value = nodes.value.map((item) => (item.key === payload.key ? { ...item, x: payload.x, y: payload.y } : item));
          log(engine, 'move-node', payload);
        },
        onConnect: (payload: { from: string; to: string }) => {
          edges.value = [...edges.value, edge(`x_${payload.from}_${payload.to}`, payload.from, payload.to, '新增')];
          log(engine, 'connect', payload);
        },
        onDeleteSelected: () => {
          if (selectedEdgeKey.value) {
            edges.value = edges.value.filter((item) => item.key !== selectedEdgeKey.value);
          } else if (selectedKey.value) {
            nodes.value = nodes.value.filter((item) => item.key !== selectedKey.value);
          }
          selectedKey.value = null;
          selectedEdgeKey.value = null;
          log(engine, 'delete-selected', null);
        },
        onNudge: (payload: { key: string; dx: number; dy: number }) => {
          nodes.value = nodes.value.map((item) => (item.key === payload.key ? { ...item, x: item.x + payload.dx, y: item.y + payload.dy } : item));
          log(engine, 'nudge', payload);
        },
        onUndo: () => log(engine, 'undo', null),
        onRedo: () => log(engine, 'redo', null),
        onAutoLayout: () => log(engine, 'auto-layout', null),
        onUndoLayout: () => log(engine, 'undo-layout', null),
      }) as Record<string, unknown>,
  };
}

const App = {
  setup() {
    const native = canvasProps('native');
    const vue = canvasProps('vueflow');
    const panes: Array<{ tag: string; engine: 'native' | 'vueflow'; state: ReturnType<typeof canvasProps> }> = [
      { tag: '老画布 native', engine: 'native', state: native },
      { tag: 'Vue Flow', engine: 'vueflow', state: vue },
    ];
    // ★ 渲染函数里**每次重新求值** props/listeners：写在外层只算一次，
    //   父组件的 selectedKey 变化就传不进画布（选中态测试会假红）。
    return () =>
      h('div', { class: 'bench' }, [
        ...panes.map((pane) =>
          h('div', { class: 'pane', 'data-pane': pane.engine, style: { width: `${scene.w}px`, height: `${scene.h}px` } }, [
            h('span', { class: 'pane__tag' }, pane.tag),
            h(pane.engine === 'native' ? NativeCanvas : VueFlowCanvas, {
              ...pane.state.props(),
              ...pane.state.listeners(),
            }),
          ]),
        ),
        h('pre', { id: 'events' }, '[]'),
      ]);
  },
};

const app = createApp(App);
app.use(TDesignIcons);
app.mount('#app');

void nextTick(() => {
  // 画布挂载 + 首帧渲染（Vue Flow 要等节点测量）
  setTimeout(() => {
    (window as unknown as { __ready: boolean }).__ready = true;
  }, 600);
});

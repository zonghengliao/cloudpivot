/**
 * 流程图纯函数与命令栈（批次 D · D5 编辑器底座）。
 *
 * 这里放**不含 Vue 的纯逻辑**，编辑器页面与画布组件共用同一份：
 * 节点几何 / 正交连线路径 / 10 px 网格吸附 / 自动分层布局 / 撤销命令栈。
 *
 * # 为什么单独抽一层
 *
 * 「画布怎么画」和「图怎么改」是两件事。几何与布局放进纯函数后，
 * 布局算法、连线路径都能在页面外单测（也便于缩放 / 只读预览复用同一套坐标）。
 *
 * # 坐标口径（架构 §11.2）
 *
 * `FlowNode.x/y` 是**画布逻辑坐标**，落库就是这个值；节点的宽高是**常量**，
 * 因此「右侧出口锚点 = (x + W, y + H/2)」这类计算不需要测量 DOM。
 * 屏幕 → 画布的换算在 `FlowCanvas.vue` 里做（那一层才有 DOM 变换矩阵）。
 *
 * # 颜色纪律
 *
 * 本文件不出现任何颜色字面量：节点的视觉样式全部在 `FlowCanvas.vue` 的
 * `<style>` 里用 `--td-*` / `--app-*` 变量表达。
 */
import type { FlowEdge, FlowNode, FlowNodeType } from '@/api/business/flow-api';
import { DEFAULT_TASK_CONFIG } from '@/views/business/flow/components/task-node-config';

/** 画布坐标点 */
export interface Point {
  /** x */
  x: number;
  /** y */
  y: number;
}

/** 节点尺寸（画布逻辑像素） */
export interface NodeSize {
  /** 宽 */
  w: number;
  /** 高 */
  h: number;
}

/** 画布网格（落点吸附；架构 §11.3 的 `snapGrid`） */
export const GRID = 10;

/** 缩放上下限（卡面验收：0.3–2.0） */
export const MIN_SCALE = 0.3;

/** 缩放上限 */
export const MAX_SCALE = 2;

/** 撤销历史上限（卡面要求 ≥ 50 步，这里给 100） */
export const HISTORY_LIMIT = 100;

/** 连线允许的最大自环/重复拦截文案 */
export const EDGE_REJECT_TEXT = {
  selfLoop: '不能连到自己（自环）',
  duplicate: '这两个节点之间已经有连线了',
  missing: '连线两端必须都在画布上',
} as const;

/** 节点类型元信息（形状 + 中文名 + 默认节点名） */
export interface NodeTypeMeta {
  /** 类型字面量 */
  type: FlowNodeType;
  /** 中文名（左侧面板 / 属性面板标题） */
  label: string;
  /** 形状：胶囊 / 矩形 / 菱形 */
  shape: 'capsule' | 'rect' | 'diamond';
  /** 新建节点时的默认名称 */
  defaultName: string;
  /** 尺寸 */
  size: NodeSize;
  /** 左侧面板的一行说明 */
  hint: string;
}

/** 七种节点（顺序 = 左侧面板顺序；与后端 `graph::NodeType` 逐字对应）
 *
 * P-FLOW-1C 追加 `TASK_NODE`（生成工作任务）与 `NOTIFY_NODE`（发通知）。
 * 两者在发布校验里分别是：`TASK_NODE` 需要 `config.task`（R17/R18），
 * `NOTIFY_NODE` 需要 `config.notify`（R17 出度 = 1、R19 配置完整性）。
 * P-FLOW-2 已补上通知节点的引擎分支，属性面板里有完整的配置表单（`NotifyNodeConfig.vue`）。
 */
export const NODE_TYPES: NodeTypeMeta[] = [
  { type: 'START', label: '开始', shape: 'capsule', defaultName: '开始', size: { w: 96, h: 44 }, hint: '流程唯一入口（有且仅有一个）' },
  { type: 'TASK_NODE', label: '任务', shape: 'rect', defaultName: '任务', size: { w: 150, h: 64 }, hint: '节点激活即生成一条工作任务' },
  { type: 'APPROVAL', label: '审批', shape: 'rect', defaultName: '审批', size: { w: 150, h: 64 }, hint: '单人会签点，出度必须为 1' },
  { type: 'COUNTERSIGN', label: '会签', shape: 'rect', defaultName: '会签', size: { w: 150, h: 64 }, hint: '多人会签（全部 / 按比例 / 一票否决）' },
  { type: 'CONDITION', label: '条件', shape: 'diamond', defaultName: '条件判断', size: { w: 132, h: 84 }, hint: '分支网关，出边需一条兜底' },
  { type: 'PARALLEL', label: '并行', shape: 'diamond', defaultName: '并行网关', size: { w: 132, h: 84 }, hint: '并行分叉 / 汇聚' },
  { type: 'CC', label: '抄送', shape: 'rect', defaultName: '抄送', size: { w: 140, h: 60 }, hint: '只读知会，不出审批任务' },
  { type: 'NOTIFY_NODE', label: '通知', shape: 'rect', defaultName: '通知', size: { w: 140, h: 60 }, hint: '发通知（发完即通过，不阻塞流程）' },
  { type: 'END', label: '结束', shape: 'capsule', defaultName: '结束', size: { w: 96, h: 44 }, hint: '流程出口（至少一个）' },
];

/** 类型 → 元信息（未知类型兜底成审批矩形，保证后端新增类型时画布不白屏） */
export const NODE_META: Record<string, NodeTypeMeta> = NODE_TYPES.reduce<Record<string, NodeTypeMeta>>((acc, meta) => {
  acc[meta.type] = meta;
  return acc;
}, {});

/** 取节点尺寸（未知类型走 APPROVAL 尺寸） */
export function nodeSize(type: string): NodeSize {
  return NODE_META[type]?.size ?? NODE_META.APPROVAL.size;
}

/** 取节点中文名（未知类型显示类型字面量） */
export function nodeTypeLabel(type: string): string {
  return NODE_META[type]?.label ?? type;
}

/** 审批类节点（出度必须 = 1；架构 §3.2 硬约束 1） */
export const APPROVAL_TYPES: string[] = ['APPROVAL', 'COUNTERSIGN', 'CC'];

/**
 * 需要配「审批人 / 负责人」的类型（P-FLOW-1C 追加 `TASK_NODE` / `NOTIFY_NODE`）。
 *
 * 与 `flow-node-caps.ts::CAPS[*].canHaveAssignee` 同口径，但这里不能 import 它 ——
 * `graph.ts` 是**不含组件层**的纯几何/命令栈模块（`FlowCanvas.vue` 反过来依赖它），
 * 互相 import 会成环。两处的一致性由单测钉住（`tests/unit/flow-task-node.test.ts`）。
 */
export const ASSIGNEE_TYPES: string[] = [...APPROVAL_TYPES, 'TASK_NODE', 'NOTIFY_NODE'];

/** 需要配置审批人的类型 */
export function needsAssignee(type: string): boolean {
  return ASSIGNEE_TYPES.includes(type);
}

/** 网格吸附 */
export function snapGrid(value: number, grid = GRID): number {
  return Math.round(value / grid) * grid;
}

/** 节点中心点 */
export function nodeCenter(node: Pick<FlowNode, 'x' | 'y' | 'type'>): Point {
  const size = nodeSize(node.type);
  return { x: node.x + size.w / 2, y: node.y + size.h / 2 };
}

/** 节点右侧出口锚点 */
export function outPort(node: Pick<FlowNode, 'x' | 'y' | 'type'>): Point {
  const size = nodeSize(node.type);
  return { x: node.x + size.w, y: node.y + size.h / 2 };
}

/** 节点左侧入口锚点 */
export function inPort(node: Pick<FlowNode, 'x' | 'y' | 'type'>): Point {
  const size = nodeSize(node.type);
  return { x: node.x, y: node.y + size.h / 2 };
}

/** 节点外接矩形（命中测试用） */
export function nodeRect(node: Pick<FlowNode, 'x' | 'y' | 'type'>): { x: number; y: number; w: number; h: number } {
  const size = nodeSize(node.type);
  return { x: node.x, y: node.y, w: size.w, h: size.h };
}

/**
 * 正交折线路径（架构 §11.4）。
 *
 * - 目标在右侧（正常流向）：`M from → L mid,fromY → L mid,toY → L to`；
 * - 目标在左侧（回流，例如退回边）：向下绕 40 px 走一圈，避免与正常边重叠；
 * - 目标与起点同列：直接上下绕行。
 *
 * # 参数
 * - `from`：起点锚点
 * - `to`：终点锚点
 */
export function edgePath(from: Point, to: Point): string {
  const gap = 40;
  // 正常向右：中段折一次
  if (to.x - from.x > gap) {
    const midX = (from.x + to.x) / 2;
    return `M ${from.x} ${from.y} L ${midX} ${from.y} L ${midX} ${to.y} L ${to.x} ${to.y}`;
  }
  // 回流 / 同列：下绕一圈（走下方通道，避开中间节点）
  const detourY = Math.max(from.y, to.y) + gap;
  return `M ${from.x} ${from.y} L ${from.x + gap / 2} ${from.y} L ${from.x + gap / 2} ${detourY} L ${to.x - gap / 2} ${detourY} L ${to.x - gap / 2} ${to.y} L ${to.x} ${to.y}`;
}

/** 折线中点（放边标签用；取两端中点即可，正交线中段是水平的） */
export function edgeLabelPoint(from: Point, to: Point): Point {
  if (to.x - from.x > 40) {
    return { x: (from.x + to.x) / 2, y: (from.y + to.y) / 2 - 8 };
  }
  const detourY = Math.max(from.y, to.y) + 40;
  return { x: (from.x + to.x) / 2, y: detourY - 8 };
}

/**
 * 命中测试：画布坐标落在哪个节点上（忽略一个排除项）。
 *
 * # 参数
 * - `nodes`：全部节点
 * - `point`：画布坐标
 * - `excludeKey`：要排除的节点 key（连线时排除自己 / 拖拽时排除自身）
 */
export function hitTest(nodes: FlowNode[], point: Point, excludeKey?: string): FlowNode | null {
  for (let i = nodes.length - 1; i >= 0; i -= 1) {
    const node = nodes[i];
    if (excludeKey && node.key === excludeKey) {
      continue;
    }
    const rect = nodeRect(node);
    if (point.x >= rect.x && point.x <= rect.x + rect.w && point.y >= rect.y && point.y <= rect.y + rect.h) {
      return node;
    }
  }
  return null;
}

/** 生成图内唯一的 key（`n_1` / `e_3`；后端 `check_key` 要求字母开头） */
export function nextKey(prefix: 'n' | 'e', used: Iterable<string>): string {
  const taken = new Set(used);
  let index = 1;
  while (taken.has(`${prefix}_${index}`)) {
    index += 1;
  }
  return `${prefix}_${index}`;
}

/** 按类型造一个新节点（默认属性与后端 `NodeSpec::new` 的默认值保持一致） */
export function makeNode(type: FlowNodeType, at: Point, used: Iterable<string>, name?: string): FlowNode {
  const meta = NODE_META[type] ?? NODE_META.APPROVAL;
  const key = nextKey('n', used);
  // 落点按「拖到哪就以哪为中心」摆放，再吸附网格
  const x = snapGrid(at.x - (type === 'START' || type === 'END' ? meta.size.w : meta.size.w / 2));
  const y = snapGrid(at.y - meta.size.h / 2);
  const isTask = type === 'TASK_NODE';
  return {
    key,
    name: name ?? meta.defaultName,
    type,
    x: Math.max(0, x),
    y: Math.max(0, y),
    sort_order: 0,
    assignee_type: needsAssignee(type) ? 'INITIATOR_SUPERIOR' : null,
    superior_levels: 1,
    form_var_key: null,
    approval_mode: type === 'COUNTERSIGN' ? 'ALL' : null,
    approve_ratio: null,
    reject_veto: true,
    // 任务节点默认「解析不出负责人就报错」（SKIP 等于这一步的活静默没人做，设计 §2.4）
    empty_assignee: isTask ? 'ERROR' : 'SKIP',
    self_approval: 'SKIP',
    allow_robot: false,
    join_mode: type === 'CONDITION' || type === 'PARALLEL' ? 'AND' : 'AND',
    fork_mode: type === 'PARALLEL' ? 'AND' : 'AND',
    timeout_hours: null,
    timeout_action: 'REMIND',
    remind_before_hours: null,
    max_remind_times: 0,
    cc_on_approved: false,
    // 任务节点自带一份 `config.task` 初值：后端 R17 要求 TASK_NODE 必须有它，
    // 拖进来就是空的会让「发布」直接 65034，用户在面板里补全即可（预演会先攒红字）
    config: isTask
      ? { task: { ...DEFAULT_TASK_CONFIG, title_template: `{{instance.biz_no}} ${name ?? meta.defaultName}` } }
      : {},
    assignees: [],
  };
}

/** 按两端节点造一条新边 */
export function makeEdge(from: string, to: string, used: Iterable<string>): FlowEdge {
  return {
    key: nextKey('e', used),
    from,
    to,
    condition: null,
    label: null,
    priority: 0,
    is_default: false,
  };
}

/**
 * 前端连线拦截（卡面验收 6：自环 / 重复边必须被拒）。
 *
 * # 参数
 * - `edges`：现有边
 * - `from` / `to`：两端节点 key
 *
 * # 返回值
 * 允许时返回 `null`；不允许时返回中文原因（页面直接 toast）。
 */
export function checkEdgeAllowed(edges: FlowEdge[], from: string, to: string): string | null {
  if (!from || !to) {
    return EDGE_REJECT_TEXT.missing;
  }
  if (from === to) {
    return EDGE_REJECT_TEXT.selfLoop;
  }
  if (edges.some((edge) => edge.from === from && edge.to === to)) {
    return EDGE_REJECT_TEXT.duplicate;
  }
  return null;
}

/**
 * 自动分层布局（按拓扑序分层，架构 §11 的「可选加分」）。
 *
 * 规则：
 * 1. 从入度为 0 的节点出发做 BFS 分层（START 天然是第 0 层）；
 * 2. 层号 = 所有前驱层号 + 1 的最大值（同层内按原 y 排序，保持用户的手工顺序）；
 * 3. 有环时（发布前必然会被后端拦）退化为「按 key 排序的第 0 层散开」，不抛错。
 *
 * # 参数
 * - `nodes` / `edges`：当前图
 *
 * # 返回值
 * `key → 新坐标` 的映射（页面把它包成一条 `layout` 命令，可撤销）
 */
export function autoLayout(nodes: FlowNode[], edges: FlowEdge[]): Map<string, Point> {
  const result = new Map<string, Point>();
  if (nodes.length === 0) {
    return result;
  }
  const indegree = new Map<string, number>();
  const successors = new Map<string, string[]>();
  for (const node of nodes) {
    indegree.set(node.key, 0);
    successors.set(node.key, []);
  }
  for (const edge of edges) {
    if (!indegree.has(edge.from) || !indegree.has(edge.to)) {
      continue;
    }
    indegree.set(edge.to, (indegree.get(edge.to) ?? 0) + 1);
    successors.get(edge.from)?.push(edge.to);
  }

  const level = new Map<string, number>();
  const queue: string[] = nodes.filter((node) => (indegree.get(node.key) ?? 0) === 0).map((node) => node.key);
  queue.forEach((key) => level.set(key, 0));
  let guard = 0;
  while (queue.length > 0 && guard < nodes.length * nodes.length + 10) {
    guard += 1;
    const key = queue.shift() as string;
    const base = level.get(key) ?? 0;
    for (const next of successors.get(key) ?? []) {
      const current = level.get(next);
      const candidate = base + 1;
      if (current === undefined || candidate > current) {
        level.set(next, candidate);
      }
      if (current === undefined) {
        queue.push(next);
      }
    }
  }
  // 环上的节点拿不到层号：统一放到最后一层，避免叠在一起
  let maxLevel = 0;
  for (const value of level.values()) {
    maxLevel = Math.max(maxLevel, value);
  }
  for (const node of nodes) {
    if (!level.has(node.key)) {
      level.set(node.key, maxLevel + 1);
    }
  }

  // 同层按原 y 再按 key 排序，让布局结果稳定（同一张图每次点结果一致）
  const columns = new Map<number, FlowNode[]>();
  for (const node of nodes) {
    const col = level.get(node.key) ?? 0;
    const list = columns.get(col) ?? [];
    list.push(node);
    columns.set(col, list);
  }
  for (const [col, list] of columns) {
    list.sort((a, b) => (a.y === b.y ? a.key.localeCompare(b.key) : a.y - b.y));
    list.forEach((node, index) => {
      result.set(node.key, { x: 60 + col * 230, y: 60 + index * 120 });
    });
  }
  return result;
}

/**
 * 按拓扑序返回节点 key 列表（从 START 出发 BFS，未连通的节点排在最后）。
 *
 * 用途：详情页把「后端只返回已到达节点」的进度，按图上的**真实先后顺序**排出来
 * （后端 `nodes_of` 的排序是 `round, created_at, node_key`，同秒创建时按 key 排，
 * 会出现「审批节点排在开始节点前面」这种反直觉顺序）。
 *
 * # 参数
 * - `nodes` / `edges`：整图
 *
 * # 返回值
 * 节点 key 数组；没有 START 时退化为「按 x 再按 y」的阅读顺序。
 */
export function topoOrder(nodes: FlowNode[], edges: FlowEdge[]): string[] {
  const byKey = new Map(nodes.map((node) => [node.key, node]));
  const successors = new Map<string, string[]>();
  const indegree = new Map<string, number>();
  for (const node of nodes) {
    successors.set(node.key, []);
    indegree.set(node.key, 0);
  }
  for (const edge of edges) {
    if (!byKey.has(edge.from) || !byKey.has(edge.to)) {
      continue;
    }
    successors.get(edge.from)?.push(edge.to);
    indegree.set(edge.to, (indegree.get(edge.to) ?? 0) + 1);
  }
  const starts = nodes.filter((node) => node.type === 'START').map((node) => node.key);
  const queue: string[] = starts.length > 0 ? [...starts] : nodes.filter((node) => (indegree.get(node.key) ?? 0) === 0).map((node) => node.key);
  const visited = new Set<string>();
  const order: string[] = [];
  while (queue.length > 0) {
    const key = queue.shift() as string;
    if (visited.has(key)) {
      continue;
    }
    visited.add(key);
    order.push(key);
    for (const next of successors.get(key) ?? []) {
      if (!visited.has(next)) {
        queue.push(next);
      }
    }
  }
  // 环上 / 孤立节点：按 x、y 的阅读顺序补齐，保证一个都不丢
  const rest = nodes
    .filter((node) => !visited.has(node.key))
    .sort((a, b) => (a.x === b.x ? a.y - b.y : a.x - b.x))
    .map((node) => node.key);
  return [...order, ...rest];
}

// ==================== 撤销 / 重做（命令栈） ====================

/** 一条命令（架构 §11.5 的 `Cmd`，字段名照 Vue 侧的可读性改过） */
export type FlowCommand =
  /** 移动一个节点 */
  | { kind: 'move'; key: string; from: Point; to: Point }
  /** 新增节点 */
  | { kind: 'add-node'; node: FlowNode }
  /** 删除节点（连边一起记，撤销时一起恢复） */
  | { kind: 'del-node'; node: FlowNode; index: number; edges: Array<{ edge: FlowEdge; index: number }> }
  /** 新增连线 */
  | { kind: 'add-edge'; edge: FlowEdge }
  /** 删除连线 */
  | { kind: 'del-edge'; edge: FlowEdge; index: number }
  /** 改节点属性（只记被改的字段） */
  | { kind: 'patch-node'; key: string; name: string; before: Partial<FlowNode>; after: Partial<FlowNode> }
  /** 改连线属性 */
  | { kind: 'patch-edge'; key: string; before: Partial<FlowEdge>; after: Partial<FlowEdge> }
  /** 自动布局（一批节点坐标） */
  | { kind: 'layout'; moves: Array<{ key: string; from: Point; to: Point }> };

/** 图的可变容器（页面持有；命令栈只操作它） */
export interface MutableGraph {
  /** 节点数组 */
  nodes: FlowNode[];
  /** 连线数组 */
  edges: FlowEdge[];
}

/**
 * 应用一条命令（redo 时用）。
 *
 * 所有分支都写成「幂等 + 不抛错」：找不到目标时静默跳过，
 * 避免撤销到一半因为一次手动删除把整个栈搞崩。
 */
export function applyCommand(graph: MutableGraph, command: FlowCommand): void {
  switch (command.kind) {
    case 'move':
    case 'layout': {
      const moves = command.kind === 'move' ? [{ key: command.key, to: command.to }] : command.moves;
      for (const move of moves) {
        const node = graph.nodes.find((item) => item.key === move.key);
        if (node) {
          node.x = move.to.x;
          node.y = move.to.y;
        }
      }
      break;
    }
    case 'add-node': {
      if (!graph.nodes.some((item) => item.key === command.node.key)) {
        graph.nodes.push({ ...command.node });
      }
      break;
    }
    case 'del-node': {
      graph.nodes = graph.nodes.filter((item) => item.key !== command.node.key);
      graph.edges = graph.edges.filter((edge) => edge.from !== command.node.key && edge.to !== command.node.key);
      break;
    }
    case 'add-edge': {
      if (!graph.edges.some((item) => item.key === command.edge.key)) {
        graph.edges.push({ ...command.edge });
      }
      break;
    }
    case 'del-edge': {
      graph.edges = graph.edges.filter((item) => item.key !== command.edge.key);
      break;
    }
    case 'patch-node': {
      const node = graph.nodes.find((item) => item.key === command.key);
      if (node) {
        Object.assign(node, command.after);
      }
      break;
    }
    case 'patch-edge': {
      const edge = graph.edges.find((item) => item.key === command.key);
      if (edge) {
        Object.assign(edge, command.after);
      }
      break;
    }
    default:
      break;
  }
}

/**
 * 反向应用一条命令（undo 时用）。
 *
 * 删除节点的反向要**按原下标插回**：顺手把边也插回原位置，
 * 否则撤销后连同线的绘制顺序都变了（视觉上「线跳了一下」）。
 */
export function revertCommand(graph: MutableGraph, command: FlowCommand): void {
  switch (command.kind) {
    case 'move': {
      const node = graph.nodes.find((item) => item.key === command.key);
      if (node) {
        node.x = command.from.x;
        node.y = command.from.y;
      }
      break;
    }
    case 'layout': {
      for (const move of command.moves) {
        const node = graph.nodes.find((item) => item.key === move.key);
        if (node) {
          node.x = move.from.x;
          node.y = move.from.y;
        }
      }
      break;
    }
    case 'add-node': {
      graph.nodes = graph.nodes.filter((item) => item.key !== command.node.key);
      graph.edges = graph.edges.filter((edge) => edge.from !== command.node.key && edge.to !== command.node.key);
      break;
    }
    case 'del-node': {
      if (!graph.nodes.some((item) => item.key === command.node.key)) {
        graph.nodes.splice(Math.min(command.index, graph.nodes.length), 0, { ...command.node });
      }
      for (const item of command.edges) {
        if (!graph.edges.some((edge) => edge.key === item.edge.key)) {
          graph.edges.splice(Math.min(item.index, graph.edges.length), 0, { ...item.edge });
        }
      }
      break;
    }
    case 'add-edge': {
      graph.edges = graph.edges.filter((item) => item.key !== command.edge.key);
      break;
    }
    case 'del-edge': {
      if (!graph.edges.some((item) => item.key === command.edge.key)) {
        graph.edges.splice(Math.min(command.index, graph.edges.length), 0, { ...command.edge });
      }
      break;
    }
    case 'patch-node': {
      const node = graph.nodes.find((item) => item.key === command.key);
      if (node) {
        Object.assign(node, command.before);
      }
      break;
    }
    case 'patch-edge': {
      const edge = graph.edges.find((item) => item.key === command.key);
      if (edge) {
        Object.assign(edge, command.before);
      }
      break;
    }
    default:
      break;
  }
}

/** 命令栈（撤销 / 重做） */
export class FlowHistory {
  /** 可撤销命令（最新在尾部） */
  private undoStack: FlowCommand[] = [];

  /** 可重做命令（最新在尾部） */
  private redoStack: FlowCommand[] = [];

  /** 上限 */
  private readonly limit: number;

  constructor(limit: number = HISTORY_LIMIT) {
    this.limit = limit;
  }

  /** 记一条命令（会清空重做栈；超过上限丢最旧的） */
  push(command: FlowCommand): void {
    this.undoStack.push(command);
    if (this.undoStack.length > this.limit) {
      this.undoStack.shift();
    }
    this.redoStack = [];
  }

  /** 取一条待撤销命令（同时压入重做栈） */
  popUndo(): FlowCommand | undefined {
    const command = this.undoStack.pop();
    if (command) {
      this.redoStack.push(command);
    }
    return command;
  }

  /** 取一条待重做命令（同时压回撤销栈） */
  popRedo(): FlowCommand | undefined {
    const command = this.redoStack.pop();
    if (command) {
      this.undoStack.push(command);
    }
    return command;
  }

  /** 是否能撤销 */
  get canUndo(): boolean {
    return this.undoStack.length > 0;
  }

  /** 是否能重做 */
  get canRedo(): boolean {
    return this.redoStack.length > 0;
  }

  /** 撤销栈深度（页面显示「可撤销 N 步」） */
  get undoDepth(): number {
    return this.undoStack.length;
  }

  /** 清空（加载新图 / 保存成功后按需清理） */
  clear(): void {
    this.undoStack = [];
    this.redoStack = [];
  }
}

<template>
  <div
    ref="wrapRef"
    class="flow-canvas"
    :class="{ 'is-readonly': readonly, 'is-panning': interacting === 'pan' }"
    tabindex="0"
    @pointerdown="onBackgroundDown"
    @wheel.prevent="onWheel"
    @keydown="onKeydown"
    @contextmenu.prevent
  >
    <svg class="flow-canvas__svg" width="100%" height="100%">
      <defs>
        <!-- 20 px 细网格（颜色走主题变量，22 套配色下都可见） -->
        <pattern id="flow-grid-pattern" width="20" height="20" patternUnits="userSpaceOnUse">
          <path class="flow-canvas__grid-line" d="M 20 0 L 0 0 0 20" fill="none" />
        </pattern>
        <!-- 箭头：orthogonal 折线的终点方向由 marker 自动旋转 -->
        <marker
          id="flow-arrow"
          viewBox="0 0 10 8"
          refX="9"
          refY="4"
          markerWidth="9"
          markerHeight="8"
          orient="auto-start-reverse"
        >
          <path class="flow-canvas__arrow" d="M 0 0 L 10 4 L 0 8 z" />
        </marker>
        <marker
          id="flow-arrow-taken"
          viewBox="0 0 10 8"
          refX="9"
          refY="4"
          markerWidth="9"
          markerHeight="8"
          orient="auto-start-reverse"
        >
          <path class="flow-canvas__arrow flow-canvas__arrow--taken" d="M 0 0 L 10 4 L 0 8 z" />
        </marker>
      </defs>

      <rect class="flow-canvas__grid" width="100%" height="100%" fill="url(#flow-grid-pattern)" />

      <!-- 全部图形都在这一个 <g> 里：平移 + 缩放只改它的 transform，节点 x/y 一个字节都不动 -->
      <g ref="sceneRef" :transform="`translate(${view.x},${view.y}) scale(${view.scale})`">
        <!-- ① 连线 -->
        <g class="flow-canvas__edges">
          <g v-for="item in edgeViews" :key="item.edge.key" class="flow-edge" :class="item.cls" :data-edge-key="item.edge.key">
            <path class="flow-edge__hit" :d="item.d" @pointerdown.stop="onEdgeDown(item.edge)" />
            <path
              class="flow-edge__line"
              :d="item.d"
              fill="none"
              :marker-end="item.isTaken ? 'url(#flow-arrow-taken)' : 'url(#flow-arrow)'"
            />
            <g v-if="item.label" class="flow-edge__label">
              <rect
                :x="item.labelX - item.labelWidth / 2 - 4"
                :y="item.labelY - 9"
                :width="item.labelWidth + 8"
                height="18"
                rx="4"
              />
              <text :x="item.labelX" :y="item.labelY + 4">{{ item.label }}</text>
            </g>
          </g>
        </g>

        <!-- ② 拖拽中的连线（虚线跟随鼠标） -->
        <path v-if="connectDraft" class="flow-canvas__draft" :d="connectDraft" fill="none" />

        <!-- ③ 节点 -->
        <g
          v-for="node in nodeViews"
          :key="node.data.key"
          class="flow-node"
          :class="node.cls"
          :data-node-key="node.data.key"
          :data-node-type="node.data.type"
          :transform="`translate(${node.x},${node.y})`"
          @pointerdown.stop="onNodeDown($event, node.data)"
        >
          <!-- 形状：胶囊用大圆角矩形，菱形用多边形，会签是「矩形 + 内框」 -->
          <polygon v-if="node.shape === 'diamond'" class="flow-node__shape" :points="node.diamondPoints" />
          <rect
            v-else
            class="flow-node__shape"
            :width="node.size.w"
            :height="node.size.h"
            :rx="node.shape === 'capsule' ? node.size.h / 2 : 6"
          />
          <rect
            v-if="node.data.type === 'COUNTERSIGN'"
            class="flow-node__inner"
            :width="node.size.w - 8"
            :height="node.size.h - 8"
            x="4"
            y="4"
            rx="4"
          />

          <text class="flow-node__label" :x="node.size.w / 2" :y="node.size.h / 2 + 5">{{ node.label }}</text>
          <text
            v-if="node.subLabel"
            class="flow-node__sub"
            :x="node.size.w / 2"
            :y="node.size.h / 2 + 20"
          >
            {{ node.subLabel }}
          </text>

          <!-- 入口 / 出口锚点（只读态不显示） -->
          <template v-if="!readonly">
            <circle class="flow-node__port" :cx="0" :cy="node.size.h / 2" r="5" />
            <circle
              class="flow-node__port flow-node__port--out"
              :class="{ 'is-target': connectTargetKey === node.data.key }"
              :cx="node.size.w"
              :cy="node.size.h / 2"
              r="5"
              @pointerdown.stop="onPortDown($event, node.data)"
            />
          </template>

          <!-- 校验问题角标 -->
          <g v-if="issueSet.has(node.data.key)" class="flow-node__issue" :transform="`translate(${node.size.w - 4},4)`">
            <circle r="8" />
            <text y="4">!</text>
          </g>
        </g>
      </g>
    </svg>

    <!-- 工具栏：缩放 / 适配（只读态隐藏） -->
    <div v-if="showToolbar" class="flow-canvas__toolbar">
      <t-button size="small" variant="outline" title="放大" @click="zoomBy(1.25)">
        <t-icon name="add" />
      </t-button>
      <t-button size="small" variant="outline" title="缩小" @click="zoomBy(0.8)">
        <t-icon name="minus" />
      </t-button>
      <span class="flow-canvas__scale">{{ Math.round(view.scale * 100) }}%</span>
      <t-button size="small" variant="outline" title="适应画布" @click="fitView()">适配</t-button>
    </div>

    <div v-if="showHint" class="flow-canvas__hint">
      拖节点移动 · 从右侧圆点拖出连线 · 滚轮缩放 · 空格/空白处拖拽平移 · Delete 删除
    </div>

    <div v-if="nodes.length === 0" class="flow-canvas__empty">
      <t-icon name="add-rectangle" size="28px" />
      <p>从左侧面板拖一个节点到画布开始</p>
    </div>
  </div>
</template>

<script setup lang="ts">
/**
 * 纯 SVG 自绘流程画布（零新增依赖，架构 §11）。
 *
 * # 三层坐标（§11.2）
 *
 * | 层 | 谁用 |
 * |----|------|
 * | 画布坐标（`node.x/y`） | 落库、连线起终点计算 |
 * | 屏幕坐标（`clientX/Y`） | pointer 事件 |
 * | 视觉变换 | 本组件的 `view = {x, y, scale}`，只作用在 `<g transform>` 上 |
 *
 * 屏幕 → 画布的换算是**精确的**：
 * `canvas = (client - 容器矩形) / scale - translate`。
 * 因为这里**不用 `viewBox`**（平移/缩放全部落在 `<g>` 上），
 * 也就不存在 `viewBox minX ≠ 0` 或 `preserveAspectRatio` 非均匀缩放导致的偏差。
 *
 * # 拖拽为什么用 pointer events（§11.3）
 *
 * HTML5 DnD 的 `drop` 事件只给 `clientX/Y` 且浏览器会做拖影，无法精确到画布坐标；
 * pointer 事件配合 `requestAnimationFrame` 节流才能稳定 60 fps（验收 4：无 >50ms 长任务）。
 *
 * # 只读态
 *
 * `readonly` 时：不画锚点、不响应拖拽/连线/删除，只保留选中、缩放、平移。
 * 已发布版本走这个分支（红线：发布后编辑器只读）。
 */
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import type { FlowEdge, FlowNode } from '@/api/business/flow-api';
import {
  GRID,
  MAX_SCALE,
  MIN_SCALE,
  NODE_META,
  edgeLabelPoint,
  edgePath,
  hitTest,
  inPort,
  nodeRect,
  outPort,
  snapGrid,
  type Point,
} from '@/views/business/flow/graph';

/** 组件入参 */
const props = withDefaults(
  defineProps<{
    /** 节点 */
    nodes: FlowNode[];
    /** 连线 */
    edges: FlowEdge[];
    /** 只读（已发布版本 / 预览） */
    readonly?: boolean;
    /** 选中的节点 key */
    selectedKey?: string | null;
    /** 选中的连线 key */
    selectedEdgeKey?: string | null;
    /** 后端校验命中、需要高亮的节点 key */
    issueNodeKeys?: string[];
    /** 是否显示缩放工具栏 */
    showToolbar?: boolean;
    /** 是否显示底部操作提示 */
    showHint?: boolean;
    /** 节点运行状态（预览 / 进度叠加：`APPROVED|ACTIVE|REJECTED|SKIPPED|WAITING`） */
    nodeStatus?: Record<string, string>;
    /** 已走过的边（`${from}>${to}`），用于进度图高亮 */
    takenPairs?: string[];
    /** 挂载后自动适应画布 */
    fitOnMount?: boolean;
  }>(),
  {
    readonly: false,
    selectedKey: null,
    selectedEdgeKey: null,
    issueNodeKeys: () => [],
    showToolbar: true,
    showHint: true,
    nodeStatus: () => ({}),
    takenPairs: () => [],
    fitOnMount: true,
  },
);

/** 组件事件 */
const emit = defineEmits<{
  /** 选中节点（null = 取消选中） */
  (event: 'select-node', key: string | null): void;
  /** 选中连线（null = 取消选中） */
  (event: 'select-edge', key: string | null): void;
  /** 拖拽结束，提交一次坐标变更（父组件据此压一条 move 命令） */
  (event: 'move-node', payload: { key: string; x: number; y: number }): void;
  /** 请求连线（父组件负责自环 / 重复边拦截） */
  (event: 'connect', payload: { from: string; to: string }): void;
  /** 删除当前选中（节点或连线） */
  (event: 'delete-selected'): void;
  /** 撤销 */
  (event: 'undo'): void;
  /** 重做 */
  (event: 'redo'): void;
  /** 方向键微调 */
  (event: 'nudge', payload: { key: string; dx: number; dy: number }): void;
}>();

/** 容器 DOM */
const wrapRef = ref<HTMLDivElement | null>(null);
/** 场景 <g>（transform 宿主） */
const sceneRef = ref<SVGGElement | null>(null);

/** 视图变换（平移 + 缩放） */
const view = ref({ x: 40, y: 40, scale: 1 });

/** 当前交互态 */
const interacting = ref<'idle' | 'node' | 'pan' | 'connect'>('idle');

/** 拖拽中的节点偏移（局部状态，不写回 props，避免每帧改父组件数据） */
const dragKey = ref<string | null>(null);
const dragOffset = ref<Point>({ x: 0, y: 0 });

/** 连线中的起点 / 当前点 / 命中的目标节点 */
const connectFrom = ref<string | null>(null);
const connectPoint = ref<Point | null>(null);
const connectTargetKey = ref<string | null>(null);

/** 平移起点 */
const panOrigin = ref<Point>({ x: 0, y: 0 });
const panStartView = ref<Point>({ x: 0, y: 0 });
/** 本次按下是否发生过移动（用来区分「点空白取消选中」与「拖拽平移」） */
const moved = ref(false);
/** 空格键是否按下（按住空格 + 拖拽 = 平移） */
const spaceDown = ref(false);

/** 校验命中集合 */
const issueSet = computed(() => new Set(props.issueNodeKeys ?? []));

/** 已走过的边集合 */
const takenSet = computed(() => new Set(props.takenPairs ?? []));

/** rAF 句柄（拖拽节流） */
let rafId = 0;
/** 待处理的指针位置 */
let pendingClient: { x: number; y: number } | null = null;

/**
 * 屏幕坐标 → 画布坐标。
 *
 * # 参数
 * - `clientX` / `clientY`：指针事件的屏幕坐标
 */
function screenToCanvas(clientX: number, clientY: number): Point {
  const rect = wrapRef.value?.getBoundingClientRect();
  if (!rect) {
    return { x: 0, y: 0 };
  }
  return {
    x: (clientX - rect.left - view.value.x) / view.value.scale,
    y: (clientY - rect.top - view.value.y) / view.value.scale,
  };
}

/** 节点渲染视图（拖拽中的节点套上局部偏移） */
interface NodeView {
  /** 原始数据 */
  data: FlowNode;
  /** 渲染坐标 */
  x: number;
  /** 渲染坐标 */
  y: number;
  /** 尺寸 */
  size: { w: number; h: number };
  /** 形状 */
  shape: 'capsule' | 'rect' | 'diamond';
  /** 菱形顶点串 */
  diamondPoints: string;
  /** 主文案（超长截断） */
  label: string;
  /** 副文案（会签模式 / 未配置审批人提示） */
  subLabel: string;
  /** class 列表 */
  cls: Record<string, boolean>;
}

/** 节点视图 */
const nodeViews = computed<NodeView[]>(() =>
  (props.nodes ?? []).map((node) => {
    const meta = metaOf(node.type);
    const size = meta.size;
    const offset = dragKey.value === node.key ? dragOffset.value : { x: 0, y: 0 };
    const status = props.nodeStatus?.[node.key];
    const isCountersign = node.type === 'COUNTERSIGN';
    const missingAssignee =
      (node.type === 'APPROVAL' || node.type === 'COUNTERSIGN' || node.type === 'CC') && !node.assignee_type;
    let subLabel = '';
    if (isCountersign && node.approval_mode) {
      subLabel = node.approval_mode === 'RATIO' ? `按比例 ${Math.round((node.approve_ratio ?? 0) * 100)}%` : node.approval_mode === 'ANY' ? '或签' : '全部同意';
    } else if (missingAssignee) {
      subLabel = '未配置审批人';
    } else if (node.type === 'CONDITION') {
      subLabel = '分支网关';
    } else if (node.type === 'PARALLEL') {
      subLabel = node.fork_mode === 'OR' ? '或分叉' : '并行';
    }
    return {
      data: node,
      x: node.x + offset.x,
      y: node.y + offset.y,
      size,
      shape: meta.shape,
      diamondPoints: `${size.w / 2},0 ${size.w},${size.h / 2} ${size.w / 2},${size.h} 0,${size.h / 2}`,
      label: truncate(node.name || meta.label, meta.shape === 'diamond' ? 6 : 9),
      subLabel,
      cls: {
        'is-selected': props.selectedKey === node.key,
        'has-issue': issueSet.value.has(node.key),
        'is-start': node.type === 'START',
        'is-end': node.type === 'END',
        'is-approval': node.type === 'APPROVAL',
        'is-countersign': node.type === 'COUNTERSIGN',
        'is-condition': node.type === 'CONDITION',
        'is-parallel': node.type === 'PARALLEL',
        'is-cc': node.type === 'CC',
        'is-active': status === 'ACTIVE',
        'is-approved': status === 'APPROVED',
        'is-rejected': status === 'REJECTED',
        'is-skipped': status === 'SKIPPED' || status === 'WAITING',
        'is-connect-target': connectTargetKey.value === node.key,
      },
    };
  }),
);

/** 连线视图 */
interface EdgeView {
  /** 原始数据 */
  edge: FlowEdge;
  /** 路径 */
  d: string;
  /** 标签文案（空串 = 不画） */
  label: string;
  /** 标签位置 */
  labelX: number;
  /** 标签位置 */
  labelY: number;
  /** 标签背景宽度（按字数估） */
  labelWidth: number;
  /** 是否走过 */
  isTaken: boolean;
  /** class 列表 */
  cls: Record<string, boolean>;
}

/** 连线视图（端点在节点表里找不到时整条跳过，避免脏数据把画布搞崩） */
const edgeViews = computed<EdgeView[]>(() => {
  const byKey = new Map((props.nodes ?? []).map((node) => [node.key, node]));
  const out: EdgeView[] = [];
  for (const edge of props.edges ?? []) {
    const fromNode = byKey.get(edge.from);
    const toNode = byKey.get(edge.to);
    if (!fromNode || !toNode) {
      continue;
    }
    const from = outPort(fromNode);
    const to = inPort(toNode);
    const label = edge.is_default ? '其他情况' : (edge.label ?? '');
    const point = edgeLabelPoint(from, to);
    out.push({
      edge,
      d: edgePath(from, to),
      label,
      labelX: point.x,
      labelY: point.y,
      labelWidth: label.length * 12,
      isTaken: takenSet.value.has(`${edge.from}>${edge.to}`),
      cls: {
        'is-selected': props.selectedEdgeKey === edge.key,
        'is-default': edge.is_default,
        'is-taken': takenSet.value.has(`${edge.from}>${edge.to}`),
      },
    });
  }
  return out;
});

/** 连线中的虚线路径 */
const connectDraft = computed(() => {
  if (!connectFrom.value || !connectPoint.value) {
    return '';
  }
  const node = (props.nodes ?? []).find((item) => item.key === connectFrom.value);
  if (!node) {
    return '';
  }
  const from = outPort(node);
  return `M ${from.x} ${from.y} L ${connectPoint.value.x} ${connectPoint.value.y}`;
});

/**
 * 取节点类型元信息。
 *
 * 尺寸与形状的**唯一来源**是 `graph.ts` 的 `NODE_TYPES`（连线起终点也按它算），
 * 这里只做「未知类型兜底成审批矩形」，避免画布和连线各拿一套尺寸。
 */
function metaOf(type: string): (typeof NODE_META)[string] {
  return NODE_META[type] ?? NODE_META.APPROVAL;
}

/** 文案截断（SVG 没有 text-overflow，只能自己截） */
function truncate(text: string, limit: number): string {
  const value = (text ?? '').trim();
  return value.length > limit ? `${value.slice(0, limit)}…` : value;
}

/** 画布空白处按下：平移 + 点空白取消选中 */
function onBackgroundDown(event: PointerEvent): void {
  if (event.button === 1 || event.button === 2 || spaceDown.value) {
    event.preventDefault();
  }
  wrapRef.value?.focus();
  moved.value = false;
  interacting.value = 'pan';
  panOrigin.value = { x: event.clientX, y: event.clientY };
  panStartView.value = { x: view.value.x, y: view.value.y };
  window.addEventListener('pointermove', onWindowMove);
  window.addEventListener('pointerup', onWindowUp);
  window.addEventListener('pointercancel', onWindowCancel);
}

/** 节点按下：选中 + （可写时）开始拖拽 */
function onNodeDown(event: PointerEvent, node: FlowNode): void {
  if (event.button !== 0) {
    return;
  }
  wrapRef.value?.focus();
  emit('select-node', node.key);
  if (props.readonly || spaceDown.value) {
    if (spaceDown.value) {
      onBackgroundDown(event);
    }
    return;
  }
  moved.value = false;
  interacting.value = 'node';
  dragKey.value = node.key;
  dragOffset.value = { x: 0, y: 0 };
  panOrigin.value = { x: event.clientX, y: event.clientY };
  window.addEventListener('pointermove', onWindowMove);
  window.addEventListener('pointerup', onWindowUp);
  window.addEventListener('pointercancel', onWindowCancel);
}

/** 出口锚点按下：开始连线 */
function onPortDown(event: PointerEvent, node: FlowNode): void {
  if (props.readonly || event.button !== 0) {
    return;
  }
  // 点画布内任何东西都把焦点收回来：否则 Delete / Ctrl+Z 这类键盘操作打不到画布上
  wrapRef.value?.focus();
  event.stopPropagation();
  interacting.value = 'connect';
  connectFrom.value = node.key;
  connectPoint.value = screenToCanvas(event.clientX, event.clientY);
  connectTargetKey.value = null;
  window.addEventListener('pointermove', onWindowMove);
  window.addEventListener('pointerup', onWindowUp);
  window.addEventListener('pointercancel', onWindowCancel);
}

/** 点连线：选中（命中区比视觉线宽，好点） */
function onEdgeDown(edge: FlowEdge): void {
  // 同上：选中连线后要能用 Delete 删掉它
  wrapRef.value?.focus();
  emit('select-edge', edge.key);
}

/** 全局 pointermove：按交互态分发；节点拖拽走 rAF 节流 */
function onWindowMove(event: PointerEvent): void {
  if (interacting.value === 'pan') {
    const dx = event.clientX - panOrigin.value.x;
    const dy = event.clientY - panOrigin.value.y;
    if (Math.abs(dx) > 3 || Math.abs(dy) > 3) {
      moved.value = true;
    }
    view.value = { ...view.value, x: panStartView.value.x + dx, y: panStartView.value.y + dy };
    return;
  }
  if (interacting.value === 'node') {
    pendingClient = { x: event.clientX, y: event.clientY };
    if (rafId) {
      return;
    }
    rafId = requestAnimationFrame(() => {
      rafId = 0;
      if (!pendingClient || !dragKey.value) {
        return;
      }
      const point = screenToCanvas(pendingClient.x, pendingClient.y);
      const origin = screenToCanvas(panOrigin.value.x, panOrigin.value.y);
      const dx = point.x - origin.x;
      const dy = point.y - origin.y;
      if (Math.abs(dx) > 1 || Math.abs(dy) > 1) {
        moved.value = true;
      }
      dragOffset.value = { x: snapGrid(dx), y: snapGrid(dy) };
    });
    return;
  }
  if (interacting.value === 'connect') {
    const point = screenToCanvas(event.clientX, event.clientY);
    connectPoint.value = point;
    const hit = findDropTarget(point, connectFrom.value);
    connectTargetKey.value = hit ? hit.key : null;
  }
}

/** 全局 pointerup：收尾并提交一次变更 */
function onWindowUp(): void {
  window.removeEventListener('pointermove', onWindowMove);
  window.removeEventListener('pointerup', onWindowUp);
  window.removeEventListener('pointercancel', onWindowCancel);
  if (rafId) {
    cancelAnimationFrame(rafId);
    rafId = 0;
  }
  const mode = interacting.value;
  interacting.value = 'idle';

  if (mode === 'node' && dragKey.value) {
    const key = dragKey.value;
    const offset = dragOffset.value;
    dragKey.value = null;
    if (moved.value && (offset.x !== 0 || offset.y !== 0)) {
      const node = (props.nodes ?? []).find((item) => item.key === key);
      if (node) {
        emit('move-node', { key, x: Math.max(0, node.x + offset.x), y: Math.max(0, node.y + offset.y) });
      }
    }
    dragOffset.value = { x: 0, y: 0 };
    return;
  }

  if (mode === 'connect' && connectFrom.value) {
    const from = connectFrom.value;
    const target = connectTargetKey.value;
    connectFrom.value = null;
    connectPoint.value = null;
    connectTargetKey.value = null;
    if (target) {
      emit('connect', { from, to: target });
    }
    return;
  }

  if (mode === 'pan' && !moved.value) {
    // 点空白处：取消选中
    emit('select-node', null);
    emit('select-edge', null);
  }
}

/**
 * 浏览器收走手势（pointercancel）：直接把本次交互丢弃，**不提交任何变更**。
 *
 * 典型场景是触摸板 / 触屏上「按下后被判成滚动」，若不留这个分支，
 * 会留下半条连线或一个「幽灵拖拽态」。
 */
function onWindowCancel(): void {
  window.removeEventListener('pointermove', onWindowMove);
  window.removeEventListener('pointerup', onWindowUp);
  window.removeEventListener('pointercancel', onWindowCancel);
  if (rafId) {
    cancelAnimationFrame(rafId);
    rafId = 0;
  }
  interacting.value = 'idle';
  dragKey.value = null;
  dragOffset.value = { x: 0, y: 0 };
  connectFrom.value = null;
  connectPoint.value = null;
  connectTargetKey.value = null;
}

/**
 * 找连线的落点：优先命中节点矩形，其次吸附 40 px 内的入口锚点。
 *
 * 细节（踩过的坑）：节点允许重叠，而**起点自己**往往也压在落点上（拖回去一点就压住了）。
 * 所以先按「最上层优先、跳过起点」找一遍；一个都没找到、而落点确实压在起点身上时，
 * 才把起点返回给页面 —— 由 `checkEdgeAllowed` 给出「不能连到自己（自环）」的提示。
 * 如果这里直接吞掉，用户会以为手势没生效。
 */
function findDropTarget(point: Point, excludeKey: string | null): FlowNode | null {
  const nodes = props.nodes ?? [];
  // ① 落点压在别的节点上（跳过起点）；节点可重叠，hitTest 取最上层那个
  const other = hitTest(nodes, point, excludeKey ?? undefined);
  if (other) {
    return other;
  }
  // ② 没压到别的节点，但压在起点自己身上 → 返回起点，交给页面提示「不能连到自己」
  if (excludeKey) {
    const self = nodes.find((node) => node.key === excludeKey);
    if (self) {
      const rect = nodeRect(self);
      if (point.x >= rect.x && point.x <= rect.x + rect.w && point.y >= rect.y && point.y <= rect.y + rect.h) {
        return self;
      }
    }
  }
  let nearest: FlowNode | null = null;
  let best = 40;
  for (const node of nodes) {
    if (node.key === excludeKey) {
      continue;
    }
    const port = inPort(node);
    const distance = Math.hypot(port.x - point.x, port.y - point.y);
    if (distance < best) {
      best = distance;
      nearest = node;
    }
  }
  return nearest;
}

/** 滚轮缩放：以鼠标位置为锚点（否则内容会「跑掉」） */
function onWheel(event: WheelEvent): void {
  const rect = wrapRef.value?.getBoundingClientRect();
  if (!rect) {
    return;
  }
  const anchor = screenToCanvas(event.clientX, event.clientY);
  const factor = Math.pow(1.1, -event.deltaY / 100);
  const next = Math.min(MAX_SCALE, Math.max(MIN_SCALE, view.value.scale * factor));
  if (next === view.value.scale) {
    return;
  }
  view.value = {
    scale: next,
    x: event.clientX - rect.left - anchor.x * next,
    y: event.clientY - rect.top - anchor.y * next,
  };
}

/** 键盘：Delete 删除 / Ctrl+Z 撤销 / 方向键微调 / 空格平移 */
function onKeydown(event: KeyboardEvent): void {
  const target = event.target as HTMLElement | null;
  const tag = target?.tagName?.toLowerCase();
  if (tag === 'input' || tag === 'textarea' || target?.isContentEditable) {
    return;
  }
  if (event.code === 'Space') {
    spaceDown.value = true;
    event.preventDefault();
    return;
  }
  const mod = event.ctrlKey || event.metaKey;
  if (mod && event.key.toLowerCase() === 'z') {
    event.preventDefault();
    if (event.shiftKey) {
      emit('redo');
    } else {
      emit('undo');
    }
    return;
  }
  if (mod && event.key.toLowerCase() === 'y') {
    event.preventDefault();
    emit('redo');
    return;
  }
  if ((event.key === 'Delete' || event.key === 'Backspace') && !props.readonly) {
    event.preventDefault();
    emit('delete-selected');
    return;
  }
  if (props.readonly || !props.selectedKey) {
    return;
  }
  const step = event.shiftKey ? GRID : 1;
  const map: Record<string, Point> = {
    ArrowLeft: { x: -step, y: 0 },
    ArrowRight: { x: step, y: 0 },
    ArrowUp: { x: 0, y: -step },
    ArrowDown: { x: 0, y: step },
  };
  const delta = map[event.key];
  if (delta) {
    event.preventDefault();
    emit('nudge', { key: props.selectedKey, dx: delta.x, dy: delta.y });
  }
}

/** 空格松开（window 级，避免焦点跑掉后按空格还在平移态） */
function onWindowKeyup(event: KeyboardEvent): void {
  if (event.code === 'Space') {
    spaceDown.value = false;
  }
}

/** 缩放（保持画布中心为锚点） */
function zoomBy(factor: number): void {
  const rect = wrapRef.value?.getBoundingClientRect();
  if (!rect) {
    return;
  }
  const next = Math.min(MAX_SCALE, Math.max(MIN_SCALE, view.value.scale * factor));
  const center = { x: rect.width / 2, y: rect.height / 2 };
  const anchor = { x: (center.x - view.value.x) / view.value.scale, y: (center.y - view.value.y) / view.value.scale };
  view.value = { scale: next, x: center.x - anchor.x * next, y: center.y - anchor.y * next };
}

/** 适应画布：把所有节点装进视口（留 60 px 边距） */
function fitView(): void {
  const rect = wrapRef.value?.getBoundingClientRect();
  const nodes = props.nodes ?? [];
  if (!rect || rect.width === 0 || nodes.length === 0) {
    return;
  }
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const node of nodes) {
    const meta = metaOf(node.type);
    minX = Math.min(minX, node.x);
    minY = Math.min(minY, node.y);
    maxX = Math.max(maxX, node.x + meta.size.w);
    maxY = Math.max(maxY, node.y + meta.size.h);
  }
  const padding = 60;
  const width = Math.max(1, maxX - minX);
  const height = Math.max(1, maxY - minY);
  const scale = Math.min(
    MAX_SCALE,
    Math.max(MIN_SCALE, Math.min((rect.width - padding * 2) / width, (rect.height - padding * 2) / height)),
  );
  view.value = {
    scale,
    x: (rect.width - width * scale) / 2 - minX * scale,
    y: (rect.height - height * scale) / 2 - minY * scale,
  };
}

/** 把某个节点居中（校验问题列表点击定位） */
function centerOn(key: string): void {
  const rect = wrapRef.value?.getBoundingClientRect();
  const node = (props.nodes ?? []).find((item) => item.key === key);
  if (!rect || !node) {
    return;
  }
  const meta = metaOf(node.type);
  const scale = Math.max(view.value.scale, 0.8);
  view.value = {
    scale,
    x: rect.width / 2 - (node.x + meta.size.w / 2) * scale,
    y: rect.height / 2 - (node.y + meta.size.h / 2) * scale,
  };
}

/** 重置视图 */
function resetView(): void {
  view.value = { x: 40, y: 40, scale: 1 };
}

onMounted(() => {
  window.addEventListener('keyup', onWindowKeyup);
  if (props.fitOnMount) {
    // 等一帧拿到真实容器尺寸（弹窗 / drawer 里挂载时首帧宽高可能是 0）
    requestAnimationFrame(() => fitView());
  }
});

onBeforeUnmount(() => {
  window.removeEventListener('keyup', onWindowKeyup);
  window.removeEventListener('pointermove', onWindowMove);
  window.removeEventListener('pointerup', onWindowUp);
  window.removeEventListener('pointercancel', onWindowCancel);
  if (rafId) {
    cancelAnimationFrame(rafId);
  }
});

defineExpose({
  /** 屏幕坐标 → 画布坐标（左侧面板拖入落点用） */
  screenToCanvas,
  /** 适应画布 */
  fitView,
  /** 定位到某节点 */
  centerOn,
  /** 重置视图 */
  resetView,
  /** 相对缩放 */
  zoomBy,
  /** 当前缩放比例 */
  getScale: () => view.value.scale,
});
</script>

<style scoped lang="less">
/**
 * 节点语义色全部走主题变量（架构 §11.9 / 红线：不许裸 hex）。
 *
 * 每类节点只定义 `--node-stroke` / `--node-fill` 两个局部变量，
 * 形状与文字样式统一在下面写一次 —— 22 套配色 × 明暗下都不会跑色。
 */
.flow-canvas {
  position: relative;
  width: 100%;
  height: 100%;
  overflow: hidden;
  outline: none;
  background: var(--td-bg-color-container);
  border: 1px solid var(--td-component-stroke);
  border-radius: 6px;
  cursor: default;

  &.is-panning {
    cursor: grabbing;
  }

  &__svg {
    display: block;
    width: 100%;
    height: 100%;
    touch-action: none;
    user-select: none;
  }

  &__grid {
    fill: var(--td-bg-color-secondarycontainer);
  }

  &__grid-line {
    stroke: var(--td-component-stroke);
    stroke-width: 1;
    stroke-opacity: 0.6;
  }

  &__arrow {
    fill: var(--td-text-color-placeholder);

    &--taken {
      fill: var(--td-success-color);
    }
  }

  &__draft {
    stroke: var(--td-brand-color);
    stroke-width: 2;
    stroke-dasharray: 6 4;
  }

  &__toolbar {
    position: absolute;
    right: 12px;
    bottom: 44px;
    display: flex;
    gap: 6px;
    align-items: center;
    padding: 6px 8px;
    background: var(--td-bg-color-container);
    border: 1px solid var(--td-component-stroke);
    border-radius: 6px;
    box-shadow: var(--td-shadow-1);
  }

  &__scale {
    min-width: 44px;
    font-size: 12px;
    color: var(--td-text-color-secondary);
    text-align: center;
  }

  &__hint {
    position: absolute;
    bottom: 10px;
    left: 12px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
    pointer-events: none;
  }

  &__empty {
    position: absolute;
    top: 50%;
    left: 50%;
    display: flex;
    flex-direction: column;
    gap: 8px;
    align-items: center;
    color: var(--td-text-color-placeholder);
    transform: translate(-50%, -50%);
    pointer-events: none;
  }
}

.flow-node {
  cursor: grab;

  &__shape {
    fill: var(--td-bg-color-container);
    stroke: var(--node-stroke, var(--td-brand-color));
    stroke-width: 1.5;
    transition: fill 0.15s ease;
  }

  &__inner {
    fill: none;
    stroke: var(--node-stroke, var(--td-brand-color));
    stroke-width: 1;
    stroke-opacity: 0.7;
  }

  &__label {
    font-size: 13px;
    font-weight: 500;
    fill: var(--td-text-color-primary);
    text-anchor: middle;
    pointer-events: none;
  }

  &__sub {
    font-size: 10px;
    fill: var(--td-text-color-secondary);
    text-anchor: middle;
    pointer-events: none;
  }

  &__port {
    fill: var(--td-bg-color-container);
    stroke: var(--td-brand-color);
    stroke-width: 1.5;
    cursor: crosshair;

    &--out:hover,
    &.is-target {
      fill: var(--td-success-color);
      stroke: var(--td-success-color);
    }
  }

  &__issue {
    circle {
      fill: var(--td-error-color);
    }

    text {
      font-size: 11px;
      font-weight: 700;
      fill: var(--td-text-color-anti);
      text-anchor: middle;
    }
  }

  // ---------- 七种节点的语义色 ----------
  &.is-start {
    --node-stroke: var(--td-success-color);
  }

  &.is-end {
    --node-stroke: var(--td-text-color-disabled);
  }

  &.is-approval {
    --node-stroke: var(--td-brand-color);
  }

  &.is-countersign {
    --node-stroke: var(--app-brand-secondary);
  }

  &.is-condition {
    --node-stroke: var(--td-warning-color);
  }

  &.is-parallel {
    --node-stroke: color-mix(in srgb, var(--td-brand-color-7) 55%, var(--app-danger));
  }

  &.is-cc {
    --node-stroke: var(--td-text-color-placeholder);

    .flow-node__shape {
      stroke-dasharray: 5 3;
    }
  }

  // 浅底色（不支持 color-mix 的浏览器退化成纯容器色，只是少了点层次）
  &:hover .flow-node__shape,
  &.is-selected .flow-node__shape {
    fill: var(--td-bg-color-container);
    fill: color-mix(in srgb, var(--node-stroke, var(--td-brand-color)) 12%, var(--td-bg-color-container));
  }

  &.is-selected .flow-node__shape {
    stroke-width: 2.5;
  }

  &.has-issue .flow-node__shape {
    stroke: var(--td-error-color);
    stroke-dasharray: 6 3;
    stroke-width: 2.5;
  }

  // ---------- 运行态（进度图） ----------
  &.is-approved .flow-node__shape {
    --node-stroke: var(--td-success-color);

    stroke-width: 2;
  }

  &.is-active .flow-node__shape {
    --node-stroke: var(--td-brand-color);

    stroke-width: 2.5;
    animation: flow-node-pulse 1.6s ease-in-out infinite;
  }

  &.is-rejected .flow-node__shape {
    --node-stroke: var(--td-error-color);

    stroke-width: 2;
  }

  &.is-skipped {
    opacity: 0.55;
  }

  &.is-connect-target .flow-node__shape {
    stroke: var(--td-success-color);
    stroke-width: 2.5;
  }
}

.flow-edge {
  &__hit {
    stroke: transparent;
    stroke-width: 14;
    cursor: pointer;
    pointer-events: stroke;
  }

  &__line {
    stroke: var(--td-text-color-placeholder);
    stroke-width: 1.5;
    // 视觉线不吃指针：命中交给下面那条 14px 宽的透明命中路径，
    // 否则点线时事件落在「没有点击处理」的线上，看起来像「点不中」
    pointer-events: none;
  }

  &.is-default &__line {
    stroke-dasharray: 5 3;
  }

  &.is-taken &__line {
    stroke: var(--td-success-color);
    stroke-width: 2;
  }

  &.is-selected &__line {
    stroke: var(--td-brand-color);
    stroke-width: 2.5;
  }

  &__label {
    pointer-events: none;

    rect {
      fill: var(--td-bg-color-container);
      stroke: var(--td-component-stroke);
    }

    text {
      font-size: 11px;
      fill: var(--td-text-color-secondary);
      text-anchor: middle;
    }
  }
}

@keyframes flow-node-pulse {
  0%,
  100% {
    stroke-opacity: 1;
  }

  50% {
    stroke-opacity: 0.45;
  }
}
</style>

<!--
  @deprecated VUE-FLOW-3（2026-11-04）起默认画布已切到 `FlowCanvasVueFlow.vue`（Vue Flow 基座）。

  **本文件（纯 SVG 自绘）只作为回退路径保留**，不再增加新特性：
    · 单页回退：URL 加 `?canvas=native`；
    · 整环境回退：构建期 `VITE_FLOW_CANVAS=native`；
    · 开关解析见 `flow-canvas-flag.ts`（默认值已改为 vueflow）。

  删除计划：新画布线上稳定运行 ≥1 个版本（约一周，期间无回退到 native 的记录）后，
  随下个版本删除本文件及其独占的辅助（`rounded-path.ts` 里只被它用到的分支、
  老画布专用的 `<marker>/<pattern>` 定义）。迁移与验证证据见
  `docs/architecture/流程画布-VueFlow迁移与清理计划.md`。

  ⚠ 删除前必须先确认：`tests/unit/flow-canvas-vueflow-contract.test.ts` 里对老画布的
  契约比对用例同步下线（那些用例是为了「切换期间两套实现等价」而存在的，不是永久需求）。
-->
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
    <svg class="flow-canvas__svg" width="100%" height="100%" :style="sceneStyle">
      <defs>
        <!-- 点阵背景（18 px 网点）：patternTransform 跟着 view 走 —— 平移 / 缩放时网点跟内容一起动，
             观感才是「在画布上移动」而不是「内容在静态玻璃后面飘」 -->
        <pattern
          id="flow-grid-pattern"
          width="18"
          height="18"
          patternUnits="userSpaceOnUse"
          :patternTransform="`translate(${view.x},${view.y}) scale(${view.scale})`"
        >
          <circle class="flow-canvas__grid-dot" cx="2" cy="2" r="1.15" />
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

      <!-- 全部图形都在这一个 <g> 里：平移 + 缩放只改**外层 svg 的 CSS transform**，节点 x/y 一个字节都不动。

           ★ SAFARI-FLOW：这个 <g> 上**不能**再写 transform。
           WebKit（Safari）对「祖先 transform + foreignObject 里的 HTML」只做布局、不做绘制：
           getBoundingClientRect 全对，但卡片实际被画在**未变换**的位置（缩放到 0.75 时整片卡片
           错位；原先节点 g 上再加 translate 时就是 6 张卡全糊在画布原点叠成一堆）。
           把 view 的 translate/scale 换成 svg 元素自己的 CSS transform（transform-origin: 0 0）后，
           整个 svg 作为一个普通 HTML 盒子被合成，WebKit 与 Chrome 的绘制位置都与布局一致
           （2026-11 实测：scripts/verify/flow_canvas_diag.cjs 的 paint 校验两引擎都通过）。 -->
      <g ref="sceneRef">
        <!-- ① 连线 -->
        <g class="flow-canvas__edges">
          <g v-for="item in edgeViews" :key="item.edge.key" class="flow-edge" :class="item.cls" :data-edge-key="item.edge.key">
            <!-- 命中层：**必须 fill="none"**。SVG path 默认 fill 是黑色，圆角正交 path 的
                 二次贝塞尔会围出弧形区域 → 未设 fill 时直接在画布上画出一块块黑楔形（P15 回归）。
                 属性 + 样式两处都写，防某天样式被 scoped 改名后静默失效。 -->
            <path class="flow-edge__hit" fill="none" :d="item.d" @pointerdown.stop="onEdgeDown(item.edge)" />
            <path
              class="flow-edge__line"
              :d="item.d"
              fill="none"
              :marker-end="item.isTaken ? 'url(#flow-arrow-taken)' : 'url(#flow-arrow)'"
            />
            <g v-if="item.label" class="flow-edge__label">
              <rect
                :x="item.labelX - item.labelWidth / 2 - 4"
                :y="item.labelY - 11"
                :width="item.labelWidth + 8"
                height="15"
                rx="3"
              />
              <text :x="item.labelX" :y="item.labelY">{{ item.label }}</text>
            </g>
          </g>
        </g>

        <!-- ② 拖拽中的连线（虚线跟随鼠标） -->
        <path v-if="connectDraft" class="flow-canvas__draft" :d="connectDraft" fill="none" />

        <!-- ③ 节点：卡片本体走 foreignObject（HTML 排版换行 / 省略 / 圆角 / 阴影都比手搓 SVG 稳），
             锚点与校验角标仍是 SVG（要精确的圆和描边）。

             ★ SAFARI-FLOW：节点坐标**不再**用 `<g transform="translate(x,y)">`，而是直接落在
             foreignObject 的 `x/y`（以及锚点 / 角标的绝对坐标）上。
             Safari（WebKit）在「<g transform> 包 <foreignObject>」这个组合下，foreignObject 里的
             HTML 可能不跟随外层 g 的 translate → 所有卡片塌陷到画布左上角叠在一起
             （2026-11 用户真机实测：连线与缩略图正常、只有卡片跑位）。
             foreignObject 的 x/y 与 <g transform> 在同级坐标系里完全等价（Chrome 下几何一致，
             有 scripts/verify/flow_canvas_diag.cjs 做双引擎对照），但少了一层 Safari 处理不好的组合。
             新增节点子元素时请同样用 `node.x / node.y` 做偏移，不要再给这个 <g> 加 transform。 -->
        <g
          v-for="node in nodeViews"
          :key="node.data.key"
          class="flow-node"
          :class="node.cls"
          :data-node-key="node.data.key"
          :data-node-type="node.data.type"
          @pointerdown.stop="onNodeDown($event, node.data)"
        >
          <foreignObject class="flow-node__fo" :x="node.x" :y="node.y" :width="node.size.w" :height="node.size.h">
            <div xmlns="http://www.w3.org/1999/xhtml" class="flow-node__card">
              <span class="flow-node__bar" />
              <span class="flow-node__icon">{{ node.icon }}</span>
              <span class="flow-node__text">
                <span class="flow-node__title">{{ node.label }}</span>
                <span v-if="node.subLabel" class="flow-node__sub">{{ node.subLabel }}</span>
              </span>
              <span
                v-if="node.hasStatus"
                class="flow-node__status"
                :class="`flow-node__status--${node.statusTone}`"
                >{{ node.statusMark }}</span
              >
            </div>
          </foreignObject>

          <!-- 命中层：卡片是 HTML（foreignObject 里的 div），它在 WebKit 里会盖住同位置的 SVG 锚点
               → 从锚点拉线会变成「拖节点」。所以命中统一交给这层透明 rect（与卡片同矩形、同一个 <g>，
               事件照旧冒泡到 onNodeDown），HTML 卡片本身 pointer-events: none（见样式）。
               Chrome 下点到的也是这层，行为与改前一致（改前点的是卡片本身）。 -->
          <rect
            class="flow-node__hit"
            :x="node.x"
            :y="node.y"
            :width="node.size.w"
            :height="node.size.h"
            fill="transparent"
          />

          <!-- 入口 / 出口锚点（只读态不显示）：坐标是「画布绝对坐标」，不再依赖外层 g 的 translate -->
          <template v-if="!readonly">
            <circle class="flow-node__port" :cx="node.x" :cy="node.y + node.size.h / 2" r="5" />
            <circle
              class="flow-node__port flow-node__port--out"
              :class="{ 'is-target': connectTargetKey === node.data.key }"
              :cx="node.x + node.size.w"
              :cy="node.y + node.size.h / 2"
              r="5"
              @pointerdown.stop="onPortDown($event, node.data)"
            />
          </template>

          <!-- 校验问题角标（同样用绝对坐标：右上角往里 4px） -->
          <g v-if="issueSet.has(node.data.key)" class="flow-node__issue">
            <circle :cx="node.x + node.size.w - 4" :cy="node.y + 4" r="8" />
            <text :x="node.x + node.size.w - 4" :y="node.y + 8">!</text>
          </g>
        </g>
      </g>
    </svg>

    <!-- 布局动作（右上角）：自动布局 / 撤销布局（只在刚布局后出现） -->
    <div v-if="showToolbar && !readonly" class="flow-canvas__layout-actions">
      <button type="button" class="flow-canvas__action" title="按 dagre 重排所有节点（可撤销）" @click.stop="emit('auto-layout')">
        <t-icon name="layout" />
        <span>自动布局</span>
      </button>
      <button
        v-if="canUndoLayout"
        type="button"
        class="flow-canvas__action flow-canvas__action--undo"
        title="把节点坐标还原成布局前的样子"
        @click.stop="emit('undo-layout')"
      >
        <t-icon name="rollback" />
        <span>撤销布局</span>
      </button>
    </div>

    <!-- 缩放控件（右下角）：+ / − / 适应画布 / 百分比（点击回到 100%） -->
    <div v-if="showToolbar" class="flow-canvas__zoom">
      <button type="button" class="flow-canvas__zoom-btn" title="放大" data-zoom="in" @click.stop="zoomBy(1.2)">
        <t-icon name="add" />
      </button>
      <button type="button" class="flow-canvas__zoom-btn" title="缩小" data-zoom="out" @click.stop="zoomBy(1 / 1.2)">
        <t-icon name="minus" />
      </button>
      <button type="button" class="flow-canvas__zoom-btn" title="适应画布" data-zoom="fit" @click.stop="fitView()">
        <t-icon name="fullscreen" />
      </button>
      <button type="button" class="flow-canvas__zoom-btn flow-canvas__zoom-btn--scale" :title="`当前 ${zoomPercent}%，点击回到 100%`" data-zoom="reset" @click.stop="zoomTo(1)">
        {{ zoomPercent }}%
      </button>
    </div>

    <!-- 小地图（左下角）：节点色块 + 视野框，点 / 拖都能平移 -->
    <div
      v-if="showToolbar && showMinimap && nodeViews.length > 0"
      ref="miniRef"
      class="flow-canvas__minimap"
      data-testid="flow-minimap"
      @pointerdown.stop="onMiniDown"
    >
      <svg class="flow-canvas__minimap-svg" :viewBox="`0 0 ${MINI_W} ${MINI_H}`" :width="MINI_W" :height="MINI_H">
        <rect
          v-for="item in miniNodes"
          :key="item.key"
          class="flow-canvas__mini-node"
          :x="item.x"
          :y="item.y"
          :width="item.w"
          :height="item.h"
          rx="3"
        />
        <rect class="flow-canvas__mini-view" :x="miniView.x" :y="miniView.y" :width="miniView.w" :height="miniView.h" rx="2" />
      </svg>
    </div>

    <div v-if="showHint" class="flow-canvas__hint">
      {{
        readonly
          ? '只读模式（已发布版本）· 如需修改请点顶部「另存为新草稿」'
          : '拖节点移动 · 从右侧圆点拖出连线 · 滚轮缩放 · 空格/空白处拖拽平移 · Delete 删除'
      }}
    </div>

    <div v-if="nodes.length === 0" class="flow-canvas__empty">
      <t-icon name="add-rectangle" size="28px" />
      <p>从左侧面板拖一个节点到画布开始</p>
    </div>
  </div>
</template>

<script setup lang="ts">
/**
 * 纯 SVG 自绘流程画布（零渲染引擎依赖，P12 视觉升级版）。
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
 * # P12 视觉升级做了什么（全部不换渲染引擎、不动坐标口径）
 *
 * - 背景：20 px 线格 → 18 px 点阵（`patternTransform` 跟随 view，网点跟着内容走）；
 * - 节点：SVG 矩形/菱形 → `foreignObject` 里的 HTML 卡片（11 px 圆角 / 左侧类型色条 /
 *   图标底块 / 主副标题单行省略 / 状态点）；锚点与校验角标仍是 SVG；
 * - 连线：`graph.ts` 的正交折线在渲染层过一遍 {@link roundedEdgePath} 切成圆角，
 *   走过的边用品牌色、指向进行中节点的边跑 `stroke-dashoffset` 动画；
 * - 边标签：裸文字 → 白底小圆角标签（宽度按字符估，见 `labelTextWidth`）；
 * - 新增右下角缩放控件（+ / − / 适应 / 百分比）与左下角小地图（点、拖都能平移）。
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
import { labelTextWidth, roundedEdgePath } from '@/views/business/flow/components/rounded-path';
import { needsAssigneeOf } from '@/views/business/flow/components/flow-node-caps';
import { ON_COMPLETE_LABELS, type OnComplete } from '@/views/business/flow/components/task-node-config';

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
    /** 是否显示左下角小地图 */
    showMinimap?: boolean;
    /** 是否显示底部操作提示 */
    showHint?: boolean;
    /** 节点运行状态（预览 / 进度叠加：`APPROVED|ACTIVE|REJECTED|SKIPPED|WAITING`） */
    nodeStatus?: Record<string, string>;
    /** 已走过的边（`${from}>${to}`），用于进度图高亮 */
    takenPairs?: string[];
    /** 挂载后自动适应画布 */
    fitOnMount?: boolean;
    /** 刚做过自动布局（决定是否显示「撤销布局」） */
    canUndoLayout?: boolean;
  }>(),
  {
    readonly: false,
    selectedKey: null,
    selectedEdgeKey: null,
    issueNodeKeys: () => [],
    showToolbar: true,
    showMinimap: true,
    showHint: true,
    nodeStatus: () => ({}),
    takenPairs: () => [],
    fitOnMount: true,
    canUndoLayout: false,
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
  /** 请求自动布局（父组件负责二次确认、算坐标、压命令栈） */
  (event: 'auto-layout'): void;
  /** 撤销上一次自动布局 */
  (event: 'undo-layout'): void;
}>();

/** 容器 DOM */
const wrapRef = ref<HTMLDivElement | null>(null);
/** 场景 <g>（transform 宿主） */
const sceneRef = ref<SVGGElement | null>(null);
/** 小地图 DOM */
const miniRef = ref<HTMLDivElement | null>(null);

/** 视图变换（平移 + 缩放） */
const view = ref({ x: 40, y: 40, scale: 1 });

/** 容器尺寸（小地图算视野框要用；ResizeObserver 维护） */
const viewport = ref({ w: 0, h: 0 });

/** 当前交互态 */
const interacting = ref<'idle' | 'node' | 'pan' | 'connect' | 'mini'>('idle');

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

/** 当前缩放百分比（缩放控件显示） */
const zoomPercent = computed(() => Math.round(view.value.scale * 100));

/**
 * 画布平移 / 缩放的**承载方式**：挂在 svg 元素自己的 CSS transform 上（不是内部 `<g>`）。
 *
 * 为什么不是 `<g transform>`：WebKit 对「祖先 transform + foreignObject 里的 HTML」只更新布局、
 * 不更新绘制，卡片会画在未变换的位置（Safari 上表现为「节点全挤在画布左上角」）。
 * 换成 SVG 元素的 CSS transform 后，整个画布作为一个合成层被整体变换，两引擎的绘制都和布局一致。
 *
 * `transform-origin: 0 0` + `translate() scale()` 与 `<g transform="translate() scale()">` 完全等价
 * （点 p → translate + p × scale），所以 fitView / 小地图 / screenToCanvas 的口径一行都不用改。
 */
const sceneStyle = computed(() => ({
  transform: `translate(${view.value.x}px, ${view.value.y}px) scale(${view.value.scale})`,
  transformOrigin: '0 0',
}));

/** rAF 句柄（拖拽节流） */
let rafId = 0;
/** 待处理的指针位置 */
let pendingClient: { x: number; y: number } | null = null;
/** 容器尺寸观察器 */
let resizeObserver: ResizeObserver | null = null;

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
  /** 左上角图标字（类型语义，见 {@link NODE_GLYPH}） */
  icon: string;
  /** 主文案（超长由 CSS 省略号截） */
  label: string;
  /** 副文案（会签模式 / 未配置审批人 / 运行状态） */
  subLabel: string;
  /** 状态点内容（空串 = 不画字，但「进行中 / 待处理」照样画点） */
  statusMark: string;
  /** 是否有运行状态（有就画状态点；`进行中`/`待处理` 无字但必须有圈） */
  hasStatus: boolean;
  /** 状态点配色档 */
  statusTone: 'approved' | 'active' | 'pending' | 'rejected';
  /** class 列表 */
  cls: Record<string, boolean>;
}

/** 类型 → 左上角图标字（★ 只影响观感，不参与任何判定） */
const NODE_GLYPH: Record<string, string> = {
  START: '▶',
  APPROVAL: '◉',
  COUNTERSIGN: '⊕',
  CONDITION: '◇',
  PARALLEL: '∥',
  CC: '↗',
  END: '■',
  // P-FLOW-1C：任务用对勾（要做完的活）、通知用信封（发出去的消息）
  TASK_NODE: '☑',
  NOTIFY_NODE: '✉',
};

/** 运行状态 → 中文（副标题 / 状态点共用） */
const STATUS_TEXT: Record<string, string> = {
  APPROVED: '已通过',
  // 术语锁定（决策 4）：节点 ACTIVE 显示「处理中」，不是「审批中」—— 节点可能是任务节点
  ACTIVE: '处理中',
  REJECTED: '已驳回',
  SKIPPED: '已跳过',
  WAITING: '待处理',
  PENDING: '待处理',
};

/** 运行状态 → 状态点档位（决定底色） */
const STATUS_TONE: Record<string, NodeView['statusTone']> = {
  APPROVED: 'approved',
  ACTIVE: 'active',
  REJECTED: 'rejected',
  SKIPPED: 'pending',
  WAITING: 'pending',
  PENDING: 'pending',
};

/** 运行状态 → 状态点内容（进行中 / 待处理留空，靠底色和脉冲环表达） */
const STATUS_MARK: Record<string, string> = {
  APPROVED: '✓',
  ACTIVE: '',
  REJECTED: '✕',
  SKIPPED: '',
  WAITING: '',
  PENDING: '',
};

/** 节点视图 */
const nodeViews = computed<NodeView[]>(() =>
  (props.nodes ?? []).map((node) => {
    const meta = metaOf(node.type);
    const size = meta.size;
    const offset = dragKey.value === node.key ? dragOffset.value : { x: 0, y: 0 };
    const status = props.nodeStatus?.[node.key];
    const isCountersign = node.type === 'COUNTERSIGN';
    // 签名模式对会签与审批节点都成立（P3-06）
    const showsMode = (isCountersign || node.type === 'APPROVAL') && Boolean(node.approval_mode);
    const missingAssignee = needsAssigneeOf(node.type) && !node.assignee_type;
    // 副标题：缺审批人优先（更该被看见），其次签名模式 / 网关类型，最后运行状态
    let subLabel = '';
    if (missingAssignee) {
      subLabel = node.type === 'TASK_NODE' ? '未配置负责人' : '未配置审批人';
    } else if (showsMode) {
      subLabel =
        node.approval_mode === 'RATIO'
          ? `按比例 ${Math.round((node.approve_ratio ?? 0) * 100)}%`
          : node.approval_mode === 'ANY'
            ? '或签'
            : '全部同意';
    } else if (node.type === 'TASK_NODE') {
      // 任务节点：把「完成后怎么走」直接画在卡片上（与属性面板同一份文案）
      subLabel = ON_COMPLETE_LABELS[(node.config as { task?: { on_complete?: string } })?.task?.on_complete as OnComplete] ?? '生成任务';
    } else if (node.type === 'NOTIFY_NODE') {
      subLabel = '发通知（发完即通过）';
    } else if (node.type === 'CONDITION') {
      subLabel = '分支网关';
    } else if (node.type === 'PARALLEL') {
      subLabel = node.fork_mode === 'OR' ? '或分叉' : '并行';
    } else if (node.type === 'CC') {
      subLabel = '只读知会';
    } else if (node.type === 'START' || node.type === 'END') {
      subLabel = node.type === 'START' ? '入口' : '出口';
    }
    const statusText = status ? STATUS_TEXT[status] : '';
    if (statusText) {
      subLabel = subLabel ? `${subLabel} · ${statusText}` : statusText;
    }
    return {
      data: node,
      x: node.x + offset.x,
      y: node.y + offset.y,
      size,
      shape: meta.shape,
      icon: NODE_GLYPH[node.type] ?? NODE_GLYPH.APPROVAL,
      label: (node.name || meta.label).trim(),
      subLabel,
      statusMark: status ? (STATUS_MARK[status] ?? '') : '',
      hasStatus: Boolean(status),
      statusTone: status ? (STATUS_TONE[status] ?? 'pending') : 'pending',
      cls: {
        'is-selected': props.selectedKey === node.key,
        'has-issue': issueSet.value.has(node.key),
        // 高度 44 的胶囊（开始 / 结束）放不下图标 + 主副两行，切紧凑排版
        'is-compact': size.h < 56,
        'is-start': node.type === 'START',
        'is-end': node.type === 'END',
        'is-approval': node.type === 'APPROVAL',
        'is-countersign': node.type === 'COUNTERSIGN',
        'is-condition': node.type === 'CONDITION',
        'is-parallel': node.type === 'PARALLEL',
        'is-cc': node.type === 'CC',
        'is-task': node.type === 'TASK_NODE',
        'is-notify': node.type === 'NOTIFY_NODE',
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
  const statusOf = props.nodeStatus ?? {};
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
    const taken = takenSet.value.has(`${edge.from}>${edge.to}`);
    out.push({
      edge,
      // 正交折线 → 圆角折线：只换画法，点序列仍由 graph.ts 算
      d: roundedEdgePath(edgePath(from, to)),
      label,
      labelX: point.x,
      labelY: point.y,
      labelWidth: labelTextWidth(label),
      isTaken: taken,
      cls: {
        'is-selected': props.selectedEdgeKey === edge.key,
        'is-default': edge.is_default,
        'is-taken': taken,
        // 指向「进行中」节点的边走流动虚线（进度图里一眼看出当前走到哪）
        'is-flowing': statusOf[edge.to] === 'ACTIVE',
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

// ==================== 小地图 ====================

/** 小地图尺寸（px，与 CSS 一致） */
const MINI_W = 150;
/** 小地图高度 */
const MINI_H = 96;
/** 小地图内边距 */
const MINI_PAD = 6;
/** 小地图里色块的最小可见边长（节点再小也要看得见） */
const MINI_MIN_SIZE = 4;

/**
 * 小地图坐标系：把「节点包围盒 ∪ 当前视野框」等比缩进 150×96。
 *
 * 为什么要并上视野框：把画布拖到空白区时，若只按节点包围盒归一化，
 * 视野框会整个跑到小地图外面（看起来像「视野消失了」）。
 */
const miniBox = computed(() => {
  const nodes = props.nodes ?? [];
  const scale = view.value.scale || 1;
  const w = viewport.value.w || 1;
  const h = viewport.value.h || 1;
  const viewX = -view.value.x / scale;
  const viewY = -view.value.y / scale;
  let minX = viewX;
  let minY = viewY;
  let maxX = viewX + w / scale;
  let maxY = viewY + h / scale;
  for (const node of nodes) {
    const size = metaOf(node.type).size;
    minX = Math.min(minX, node.x);
    minY = Math.min(minY, node.y);
    maxX = Math.max(maxX, node.x + size.w);
    maxY = Math.max(maxY, node.y + size.h);
  }
  const boxW = Math.max(1, maxX - minX);
  const boxH = Math.max(1, maxY - minY);
  const scaleMini = Math.min((MINI_W - MINI_PAD * 2) / boxW, (MINI_H - MINI_PAD * 2) / boxH);
  const offsetX = MINI_PAD + (MINI_W - MINI_PAD * 2 - boxW * scaleMini) / 2;
  const offsetY = MINI_PAD + (MINI_H - MINI_PAD * 2 - boxH * scaleMini) / 2;
  return { minX, minY, scale: scaleMini, offsetX, offsetY, w, h };
});

/** 小地图里的节点色块 */
const miniNodes = computed(() => {
  const box = miniBox.value;
  return (props.nodes ?? []).map((node) => {
    const size = metaOf(node.type).size;
    return {
      key: node.key,
      x: (node.x - box.minX) * box.scale + box.offsetX,
      y: (node.y - box.minY) * box.scale + box.offsetY,
      w: Math.max(MINI_MIN_SIZE, size.w * box.scale),
      h: Math.max(MINI_MIN_SIZE, size.h * box.scale),
    };
  });
});

/** 小地图里的视野框 */
const miniView = computed(() => {
  const box = miniBox.value;
  const scale = view.value.scale || 1;
  const x = (-view.value.x / scale - box.minX) * box.scale + box.offsetX;
  const y = (-view.value.y / scale - box.minY) * box.scale + box.offsetY;
  return {
    x,
    y,
    w: Math.max(6, (box.w / scale) * box.scale),
    h: Math.max(6, (box.h / scale) * box.scale),
  };
});

/** 把小地图上的点换算成画布坐标 */
function miniToCanvas(clientX: number, clientY: number): Point | null {
  const el = miniRef.value;
  if (!el) {
    return null;
  }
  const rect = el.getBoundingClientRect();
  if (rect.width === 0 || rect.height === 0) {
    return null;
  }
  const box = miniBox.value;
  // DOM 尺寸与 viewBox 可能不等比（CSS 缩过），按比例折回 viewBox 坐标
  const mx = ((clientX - rect.left) / rect.width) * MINI_W;
  const my = ((clientY - rect.top) / rect.height) * MINI_H;
  return {
    x: (mx - box.offsetX) / box.scale + box.minX,
    y: (my - box.offsetY) / box.scale + box.minY,
  };
}

/** 让小地图上的某个画布点成为视野中心 */
function centerViewOn(point: Point): void {
  const scale = view.value.scale;
  const w = viewport.value.w || wrapRef.value?.getBoundingClientRect().width || 0;
  const h = viewport.value.h || wrapRef.value?.getBoundingClientRect().height || 0;
  if (w === 0 || h === 0) {
    return;
  }
  view.value = { ...view.value, x: w / 2 - point.x * scale, y: h / 2 - point.y * scale };
}

/** 小地图按下：直接跳到该点，之后拖动持续平移 */
function onMiniDown(event: PointerEvent): void {
  if (event.button !== 0) {
    return;
  }
  const point = miniToCanvas(event.clientX, event.clientY);
  if (!point) {
    return;
  }
  event.preventDefault();
  interacting.value = 'mini';
  centerViewOn(point);
  window.addEventListener('pointermove', onWindowMove);
  window.addEventListener('pointerup', onWindowUp);
  window.addEventListener('pointercancel', onWindowCancel);
}

/**
 * 取节点类型元信息。
 *
 * 尺寸与形状的**唯一来源**是 `graph.ts` 的 `NODE_TYPES`（连线起终点也按它算），
 * 这里只做「未知类型兜底成审批矩形」，避免画布和连线各拿一套尺寸。
 */
function metaOf(type: string): (typeof NODE_META)[string] {
  return NODE_META[type] ?? NODE_META.APPROVAL;
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
  if (interacting.value === 'mini') {
    const point = miniToCanvas(event.clientX, event.clientY);
    if (point) {
      centerViewOn(point);
    }
    return;
  }
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

  if (mode === 'mini') {
    // 小地图平移：不选中、不取消选中
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

/** 缩放到指定比例（保持画布中心为锚点） */
function zoomTo(scale: number): void {
  const rect = wrapRef.value?.getBoundingClientRect();
  if (!rect) {
    return;
  }
  const next = Math.min(MAX_SCALE, Math.max(MIN_SCALE, scale));
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
  const el = wrapRef.value;
  if (el) {
    viewport.value = { w: el.clientWidth, h: el.clientHeight };
    if (typeof ResizeObserver !== 'undefined') {
      resizeObserver = new ResizeObserver((entries) => {
        for (const entry of entries) {
          const box = entry.contentRect;
          viewport.value = { w: box.width, h: box.height };
        }
      });
      resizeObserver.observe(el);
    }
  }
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
  resizeObserver?.disconnect();
  resizeObserver = null;
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
  /** 缩放到指定比例 */
  zoomTo,
  /** 当前缩放比例 */
  getScale: () => view.value.scale,
});
</script>

<style scoped lang="less">
/**
 * 视觉规范（P12）。颜色一律走主题变量（红线：不许裸 hex）：
 *
 * - 类型色用 `--node-accent`，由各类节点 class 指到 `--td-brand-color` / 品牌色阶 /
 *   语义色上，22 套配色自动跟随；需要「同色系但更深 / 更浅」时用 `color-mix` 组合现有变量。
 * - 投影用 `--td-shadow-1/2`（跟随主色的调色投影），不写死 rgba。
 */
.flow-canvas {
  position: relative;
  width: 100%;
  height: 100%;
  overflow: hidden;
  outline: none;
  background: var(--td-bg-color-page);
  border: 1px solid var(--td-component-stroke);
  border-radius: 6px;
  cursor: default;

  // 顶部柔和品牌光晕（画在 SVG 下面：网格 rect 是透明的，光晕透得上来）
  &::before {
    content: '';
    position: absolute;
    inset: 0;
    z-index: 0;
    background: radial-gradient(
      ellipse 60% 45% at 50% 0%,
      color-mix(in srgb, var(--td-brand-color) 10%, transparent),
      transparent 70%
    );
    pointer-events: none;
  }

  &.is-panning {
    cursor: grabbing;
  }

  &__svg {
    position: relative;
    z-index: 1;
    display: block;
    width: 100%;
    height: 100%;
    // SAFARI-FLOW：平移/缩放是 svg 元素自己的 CSS transform（见 sceneStyle），
    // 缩小或平移到画布外时内容必须还能画出来 —— 由容器 .flow-canvas 的 overflow: hidden 负责裁剪。
    overflow: visible;
    touch-action: none;
    user-select: none;
  }

  // 网格 rect 只负责挂 pattern，底色交给容器（这样 ::before 光晕可见）
  &__grid {
    fill: transparent;
  }

  // 点阵：用「占位文字色 × 45%」混合，浅色 / 深色 / 22 套配色下都是一层淡淡的网点
  &__grid-dot {
    fill: color-mix(in srgb, var(--td-text-color-placeholder) 45%, transparent);
  }

  &__arrow {
    fill: var(--td-text-color-placeholder);

    &--taken {
      fill: var(--td-brand-color);
    }
  }

  &__draft {
    stroke: var(--td-brand-color);
    stroke-width: 2;
    stroke-dasharray: 6 4;
  }

  // ---------- 布局动作（右上角） ----------
  &__layout-actions {
    position: absolute;
    top: 12px;
    right: 12px;
    z-index: 3;
    display: flex;
    gap: 6px;
  }

  &__action {
    display: inline-flex;
    gap: 5px;
    align-items: center;
    padding: 6px 10px;
    font-size: 12px;
    color: var(--td-text-color-secondary);
    cursor: pointer;
    background: var(--td-bg-color-container);
    border: 1px solid var(--td-component-stroke);
    border-radius: 9px;
    box-shadow: var(--td-shadow-1);
    transition:
      background 0.16s ease,
      color 0.16s ease;

    &:hover {
      color: var(--td-brand-color);
      background: var(--td-bg-color-container-hover);
    }

    &--undo {
      color: var(--td-brand-color);
    }
  }

  // ---------- 缩放控件（右下角，竖排） ----------
  &__zoom {
    position: absolute;
    right: 16px;
    bottom: 16px;
    z-index: 3;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: var(--td-bg-color-container);
    border: 1px solid var(--td-component-stroke);
    border-radius: 10px;
    box-shadow: var(--td-shadow-2);
  }

  &__zoom-btn {
    display: grid;
    place-items: center;
    width: 34px;
    height: 32px;
    padding: 0;
    font-size: 13px;
    color: var(--td-text-color-secondary);
    cursor: pointer;
    background: transparent;
    border: none;
    border-bottom: 1px solid var(--td-component-stroke);

    &:last-child {
      border-bottom: none;
    }

    &:hover {
      color: var(--td-brand-color);
      background: var(--td-bg-color-container-hover);
    }

    &--scale {
      font-size: 10.5px;
      font-variant-numeric: tabular-nums;
    }
  }

  // ---------- 小地图（左下角） ----------
  &__minimap {
    position: absolute;
    bottom: 16px;
    left: 16px;
    z-index: 3;
    box-sizing: border-box;
    width: 150px;
    height: 96px;
    overflow: hidden;
    cursor: pointer;
    background: var(--td-bg-color-container);
    border: 1px solid var(--td-component-stroke);
    border-radius: 9px;
    box-shadow: var(--td-shadow-1);
    opacity: 0.94;
    transition: opacity 0.16s ease;

    &:hover {
      opacity: 1;
    }
  }

  &__minimap-svg {
    display: block;
    width: 100%;
    height: 100%;
  }

  &__mini-node {
    fill: var(--td-gray-color-4);
  }

  &__mini-view {
    fill: color-mix(in srgb, var(--td-brand-color) 10%, transparent);
    stroke: var(--td-brand-color);
    stroke-width: 1.5;
  }

  &__hint {
    position: absolute;
    bottom: 120px;
    left: 16px;
    z-index: 2;
    max-width: calc(100% - 220px);
    font-size: 12px;
    color: var(--td-text-color-placeholder);
    pointer-events: none;
  }

  &__empty {
    position: absolute;
    top: 50%;
    left: 50%;
    z-index: 2;
    display: flex;
    flex-direction: column;
    gap: 8px;
    align-items: center;
    color: var(--td-text-color-placeholder);
    transform: translate(-50%, -50%);
    pointer-events: none;
  }
}

// ---------- 节点卡片 ----------
.flow-node {
  cursor: grab;

  &__fo {
    overflow: visible;
  }

  &__card {
    box-sizing: border-box;
    position: relative;
    display: flex;
    gap: 8px;
    align-items: center;
    width: 100%;
    height: 100%;
    padding: 11px 13px;
    overflow: hidden;
    // 命中交给同位置的 SVG rect（.flow-node__hit）：WebKit 里 foreignObject 的 HTML 会盖住 SVG 锚点
    pointer-events: none;
    background: var(--td-bg-color-container);
    border: 1px solid var(--td-component-stroke);
    border-radius: 11px;
    box-shadow: var(--td-shadow-1);
    transition:
      box-shadow 0.18s ease,
      transform 0.18s ease,
      border-color 0.18s ease;
    user-select: none;
  }

  // 左侧类型色条
  &__bar {
    position: absolute;
    top: 8px;
    bottom: 8px;
    left: 0;
    width: 3.5px;
    background: var(--node-accent, var(--td-brand-color));
    border-radius: 3px;
  }

  // 图标底块（底色 = 类型色）
  &__icon {
    display: grid;
    flex: none;
    place-items: center;
    width: 30px;
    height: 30px;
    font-size: 15px;
    font-weight: 700;
    line-height: 1;
    color: var(--td-text-color-anti);
    background: var(--node-accent, var(--td-brand-color));
    border-radius: 9px;
  }

  &__text {
    display: flex;
    flex: 1 1 auto;
    flex-direction: column;
    min-width: 0;
  }

  &__title {
    overflow: hidden;
    font-size: 13px;
    font-weight: 600;
    line-height: 1.35;
    color: var(--td-text-color-primary);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  &__sub {
    margin-top: 1px;
    overflow: hidden;
    font-size: 11.5px;
    line-height: 1.3;
    color: var(--td-text-color-secondary);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  // 状态点
  &__status {
    position: relative;
    display: grid;
    flex: none;
    place-items: center;
    width: 17px;
    height: 17px;
    font-size: 10px;
    font-weight: 700;
    line-height: 1;
    color: var(--td-text-color-anti);
    border-radius: 50%;

    &--approved {
      background: var(--td-success-color);
    }

    &--active {
      background: var(--td-brand-color);

      // 进行中：外面加一圈扩散脉冲
      &::after {
        position: absolute;
        inset: -4px;
        content: '';
        border: 1.5px solid var(--td-brand-color);
        border-radius: 50%;
        animation: flow-status-pulse 1.6s ease-out infinite;
      }
    }

    &--pending {
      background: var(--td-text-color-placeholder);
    }

    &--rejected {
      background: var(--td-error-color);
    }
  }

  // 开始 / 结束（高 44）放不下两行，切紧凑排版
  &.is-compact {
    .flow-node__card {
      gap: 7px;
      padding: 5px 10px;
    }

    .flow-node__icon {
      width: 22px;
      height: 22px;
      font-size: 12px;
      border-radius: 7px;
    }

    .flow-node__sub {
      display: none;
    }
  }

  // ---------- 七种节点的类型色（全部走主题变量 / 色阶组合） ----------
  &.is-start {
    --node-accent: var(--td-success-color);
  }

  &.is-end {
    --node-accent: var(--td-text-color-disabled);
  }

  &.is-approval {
    --node-accent: var(--td-brand-color);
  }

  // 会签：同色系更深一档（不与审批撞色，也不写死第二种品牌色）
  &.is-countersign {
    --node-accent: color-mix(in srgb, var(--td-brand-color) 70%, var(--td-text-color-primary));
  }

  &.is-condition {
    --node-accent: var(--td-warning-color);
  }

  &.is-parallel {
    --node-accent: color-mix(in srgb, var(--td-brand-color-7) 55%, var(--td-error-color));
  }

  // 抄送：浅色阶（只读知会，视觉上最轻）
  &.is-cc {
    --node-accent: var(--td-brand-color-4);
  }

  // 任务（P-FLOW-1C）：用品类最醒目的「需要人干活」色阶 —— 与审批同族但更深，
  // 因为任务与审批在画布上经常相邻，撞色会让人以为它们是同一类节点
  &.is-task {
    --node-accent: color-mix(in srgb, var(--td-brand-color) 62%, var(--td-success-color));
  }

  // 通知：虚线感的最轻一档（不阻塞等人）
  &.is-notify {
    --node-accent: color-mix(in srgb, var(--td-warning-color) 55%, var(--td-text-color-placeholder));
  }

  // ---------- 交互态 ----------
  &:hover .flow-node__card {
    box-shadow: var(--td-shadow-2);
    transform: translateY(-2px);
  }

  &.is-selected .flow-node__card {
    border-color: var(--node-accent, var(--td-brand-color));
    box-shadow:
      0 0 0 2px color-mix(in srgb, var(--node-accent, var(--td-brand-color)) 35%, transparent),
      var(--td-shadow-2);
  }

  &.has-issue .flow-node__card {
    border-color: var(--td-error-color);
    border-style: dashed;
  }

  // ---------- 运行态（进度图） ----------
  &.is-approved .flow-node__card {
    border-color: var(--td-success-color);
  }

  &.is-active .flow-node__card {
    border-color: var(--td-brand-color);
  }

  &.is-rejected .flow-node__card {
    border-color: var(--td-error-color);
  }

  &.is-skipped {
    opacity: 0.55;
  }

  // 连线拖到它头上：虚线提示可以落点
  &.is-connect-target .flow-node__card {
    border-color: var(--td-success-color);
    border-style: dashed;
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--td-success-color) 30%, transparent);
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
}

// ---------- 连线 ----------
.flow-edge {
  &__hit {
    // ★ 少了这一行就是 P15 那个「连线变黑块」：SVG path 默认 fill 是黑色，
    // 圆角正交 path（M..L..Q..）的贝塞尔会围出弧形填充区域。属性 fill="none" 同义，双保险。
    fill: none;
    stroke: transparent;
    stroke-width: 14;
    cursor: pointer;
    // 只吃描边不吃填充：热区靠 14px 透明描边，点哪都命中
    pointer-events: stroke;
  }

  &__line {
    fill: none;
    stroke: var(--td-gray-color-5);
    stroke-width: 1.9;
    stroke-linecap: round;
    stroke-linejoin: round;
    // 视觉线不吃指针：命中交给上面那条 14px 宽的透明命中路径，
    // 否则点线时事件落在「没有点击处理」的线上，看起来像「点不中」
    pointer-events: none;
  }

  &.is-default &__line {
    stroke-dasharray: 5 3;
  }

  &.is-taken &__line {
    stroke: var(--td-brand-color);
    stroke-width: 2.4;
  }

  &.is-flowing &__line {
    stroke: var(--td-brand-color);
    stroke-dasharray: 7 7;
    animation: flow-dash 1.1s linear infinite;
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
      stroke-width: 1;
      opacity: 0.96;
    }

    text {
      font-size: 10.5px;
      fill: var(--td-text-color-secondary);
      text-anchor: middle;
    }
  }
}

@keyframes flow-dash {
  to {
    stroke-dashoffset: -14;
  }
}

@keyframes flow-status-pulse {
  0% {
    opacity: 0.85;
    transform: scale(0.85);
  }

  100% {
    opacity: 0;
    transform: scale(1.45);
  }
}
</style>

<style lang="less">
/**
 * 深色主题微调（**故意不 scoped**）。
 *
 * `html[theme-mode='dark']` 写在 scoped 块里会被编译成
 * `html[theme-mode='dark'][data-v-x] .flow-canvas[data-v-x]` —— 前者永远不匹配，
 * 规则静默失效；而 `.flow-canvas` 这个类名只有本组件用，非 scoped 也不会外溢。
 */
html[theme-mode='dark'] .flow-canvas::before {
  opacity: 0.5;
}
</style>

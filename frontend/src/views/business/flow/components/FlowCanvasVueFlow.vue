<template>
  <div
    ref="wrapRef"
    class="flow-canvas"
    :class="{ 'is-readonly': readonly, 'is-space-pan': spaceDown, 'is-connecting': isConnecting }"
    tabindex="0"
    @keydown="onKeydown"
    @pointerdown.capture="onWrapPointerDown"
    @contextmenu.prevent
  >
    <!-- 箭头 marker：与老画布同一套几何（10×8 三角 / 9×8 视框 / refX 9）。
         放在一个 0×0 的 svg 里而不是 Vue Flow 内部：`url(#id)` 是**文档级**查找，
         跨 svg 引用完全合法，这样箭头颜色仍由 CSS 类 + 主题变量决定（22 套配色自动跟随），
         不必把颜色字符串塞进 marker 定义。id 加 `vf-` 前缀，避免与老画布同时挂载时撞 id。 -->
    <svg class="flow-canvas__defs" aria-hidden="true" focusable="false">
      <defs>
        <marker id="vf-flow-arrow" viewBox="0 0 10 8" refX="9" refY="4" markerWidth="9" markerHeight="8" orient="auto-start-reverse">
          <path class="flow-canvas__arrow" d="M 0 0 L 10 4 L 0 8 z" />
        </marker>
        <marker id="vf-flow-arrow-taken" viewBox="0 0 10 8" refX="9" refY="4" markerWidth="9" markerHeight="8" orient="auto-start-reverse">
          <path class="flow-canvas__arrow flow-canvas__arrow--taken" d="M 0 0 L 10 4 L 0 8 z" />
        </marker>
      </defs>
    </svg>

    <VueFlow
      class="flow-canvas__flow"
      :nodes="vfNodes"
      :edges="vfEdges"
      :default-viewport="{ x: 40, y: 40, zoom: 1 }"
      :min-zoom="MIN_SCALE"
      :max-zoom="MAX_SCALE"
      :nodes-draggable="!readonly && !spaceDown"
      :nodes-connectable="!readonly"
      :elements-selectable="false"
      :select-nodes-on-drag="false"
      :node-drag-threshold="0"
      :delete-key-code="null"
      :selection-key-code="null"
      :multi-selection-key-code="null"
      :disable-keyboard-a11y="true"
      :zoom-on-scroll="true"
      :zoom-on-pinch="true"
      :zoom-on-double-click="false"
      :pan-on-scroll="false"
      :pan-on-drag="true"
      :prevent-scrolling="true"
      :snap-to-grid="false"
      :only-render-visible-elements="false"
      :elevate-edges-on-select="false"
      :elevate-nodes-on-select="false"
      :edges-updatable="false"
      :nodes-focusable="false"
      :edges-focusable="false"
      :auto-pan-on-connect="false"
      :auto-pan-on-node-drag="false"
      :connect-on-click="false"
      :fit-view-on-init="false"
      :connection-line-options="connectionLineOptions"
      @node-click="onNodeClick"
      @edge-click="onEdgeClick"
      @node-drag-stop="onNodeDragStop"
      @connect="onConnect"
      @connect-start="onConnectStart"
      @connect-end="onConnectEnd"
      @pane-click="onPaneClick"
      @viewport-change="onViewportChange"
      @nodes-initialized="onNodesInitialized"
    >
      <!-- 背景点阵：与外层老画布同一套参数（18px 间距 / 跟着视口走）。
           ★ 注意 @vue-flow/background 的两个上游坑（都实测过）：
             1. `size` 是**直径**（内部渲染成 `<circle r=size/2>`），老画布是 `r=1.15`；
                但老画布的网点色是「占位文字色 × 45%」，在浅色主题下本身只有 2/255 的可见度
                （实测点阵最暗 239、背景 241），传 1.15 与 2.3 的实测整图差分别是 0.11 / 0.54 ——
                反而 1.15（更淡）更接近老画布，故保持 1.15（与 VUE-FLOW-1 一致）。
             2. `offset` 传负数会被拼成 `translate(--0.85)`（字符串拼接），整条 transform 失效；
                要调相位只能用正数（mod gap）。 -->
      <Background class="flow-canvas__background" variant="dots" :gap="18" :size="1.15" pattern-color="color-mix(in srgb, var(--td-text-color-placeholder) 45%, transparent)" />

      <!-- ① 节点卡片：与老画布同一套规格（尺寸随类型、11px 圆角、3.5px 色条、30×30 图标底块）。
           ★ 卡片 / 校验角标 / Handle 三者是**兄弟**节点：
             - 卡片自带 overflow: hidden（老画布同款裁切），角标与句柄放在里面会被裁掉；
             - 它们的定位基准是 Vue Flow 的节点盒子（与卡片同尺寸，见 node + width/height），
               所以 `top/right: -4px` 与老画布的绝对坐标 (w-4, 4) 逐像素等价。 -->
      <template #node-flow="{ data }">
        <div
          class="flow-node__card"
          :class="data.cls"
          :style="{ width: `${data.w}px`, height: `${data.h}px` }"
          :data-node-key="data.key"
          :data-node-type="data.type"
        >
          <span class="flow-node__bar" />
          <span class="flow-node__icon">{{ data.icon }}</span>
          <span class="flow-node__text">
            <span class="flow-node__title">{{ data.label }}</span>
            <span v-if="data.subLabel" class="flow-node__sub">{{ data.subLabel }}</span>
          </span>
          <span v-if="data.hasStatus" class="flow-node__status" :class="`flow-node__status--${data.statusTone}`">{{ data.statusMark }}</span>
        </div>

        <!-- 校验角标：红色圆形 !（16×16，圆心落在卡片右上角往里 4px 处，与老画布 circle(w-4, 4, r=8) 等价）。
             `title` 给出可读的原因提示 —— 画布只拿到 key 列表（props.issueNodeKeys），
             详细规则名 / 文案在调用方的「校验未通过 N 条」清单里。 -->
        <span v-if="data.cls['has-issue']" class="flow-node__issue" :title="ISSUE_HINT" data-issue-badge>!</span>

        <!-- 入口 / 出口锚点：**无论只读与否都必须留在 DOM 里**。
             ★ 这是 VUE-FLOW-2 修掉的一个真机 bug：Handle 不只是「拖线的手柄」，它还是
               Vue Flow 计算 sourceX/sourceY/targetX/targetY 的**唯一依据**。
               只读态用 `v-if="!readonly"` 把 Handle 摘掉 → 锚点坐标退化成节点 bottom/top
               → 所有连线变成「底部出、顶部进」（老画布是右出左进），用户截图反馈过。
               所以只读态的做法是：元素保留 + `connectable=false` + CSS `opacity: 0` 视觉隐藏。
               （**绝不能**用 display:none / visibility:hidden / width:0 —— 一样会让锚点退化。）
             非只读：可拖拽的 Handle（圆形，半径 5px，hover 放大）。
             ★ 圆点用内嵌 SVG `<circle r=5 stroke-width=1.5>` 画，而不是 div 的 border：
               Chrome 会把 1.5px 的 border-width 取整成 1px（实测），而老画布就是这条 SVG 圆
               —— 用同一套 SVG 元素，两边的描边栅格化才逐像素一致。
             ★ `data-port-node-key` 挂在两个 Handle 上（**不是** `data-node-key`）：Vue Flow 的 Handle
               是卡片 div 的**兄弟**（不是子节点），「某节点的出口锚点」需要自己的定位属性；
               若复用 `data-node-key`，`[data-node-key]` 就会从「一节点一元素」变成 3 个，
               e2e 的节点计数全错。老画布的锚点在节点 `<g data-node-key>` 内部，于是回归用例写
               `.flow-node__port--out[data-port-node-key=k], [data-node-key=k] .flow-node__port--out`
               —— 两条画布都能命中且只有 1 个。 -->
        <Handle
          type="target"
          :position="Position.Left"
          class="flow-node__port flow-node__port--in"
          :class="{ 'is-readonly': readonly }"
          :data-port-node-key="data.key"
          :connectable="!readonly"
        >
          <svg class="flow-node__port-svg" viewBox="0 0 11.5 11.5" width="11.5" height="11.5" aria-hidden="true">
            <circle class="flow-node__port-dot" cx="5.75" cy="5.75" r="5" />
          </svg>
        </Handle>
        <Handle
          type="source"
          :position="Position.Right"
          class="flow-node__port flow-node__port--out"
          :class="{ 'is-readonly': readonly, 'is-target': connectTargetKey === data.key }"
          :data-port-node-key="data.key"
          :connectable="!readonly"
        >
          <svg class="flow-node__port-svg" viewBox="0 0 11.5 11.5" width="11.5" height="11.5" aria-hidden="true">
            <circle class="flow-node__port-dot" cx="5.75" cy="5.75" r="5" />
          </svg>
        </Handle>
      </template>

      <!-- ② 连线：正交圆角折线 + 白底小标签（复用 graph.ts / rounded-path.ts 的纯函数，与老画布同一份几何） -->
      <template #edge-flow="edgeProps">
        <g class="flow-edge" :class="edgeViewOf(edgeProps).cls">
          <!-- 命中层：透明宽描边（14px），点边选中靠它；视觉线不吃指针 -->
          <path class="flow-edge__hit" :d="edgeViewOf(edgeProps).d" fill="none" @pointerdown.stop />
          <path
            class="flow-edge__line"
            :d="edgeViewOf(edgeProps).d"
            fill="none"
            :marker-end="edgeViewOf(edgeProps).isTaken ? 'url(#vf-flow-arrow-taken)' : 'url(#vf-flow-arrow)'"
          />
          <g v-if="edgeViewOf(edgeProps).label" class="flow-edge__label">
            <rect
              :x="edgeViewOf(edgeProps).labelX - edgeViewOf(edgeProps).labelWidth / 2 - 4"
              :y="edgeViewOf(edgeProps).labelY - 11"
              :width="edgeViewOf(edgeProps).labelWidth + 8"
              height="15"
              rx="3"
            />
            <text :x="edgeViewOf(edgeProps).labelX" :y="edgeViewOf(edgeProps).labelY">{{ edgeViewOf(edgeProps).label }}</text>
          </g>
        </g>
      </template>

      <!-- ③ 小地图（左下角）：受 showToolbar && showMinimap 控制，与老画布显示条件一致。
           位置必须显式给 `bottom-left`：Vue Flow 默认在右下角，与老画布（左下）不一致。
           ★ 不用组件的 `pannable`：它的平移读 `MouseEvent.movementX/Y`，**Safari/WebKit 不支持这两个属性**
             → 在小地图上拖拽完全没反应（Chromium 正常）。这里改为自己接管指针：
             点/拖 = 「把点到的画布位置挪到视野中心」，与老画布的点按行为一致，三端一致。 -->
      <MiniMap
        v-if="showToolbar && showMinimap && nodes.length > 0"
        class="flow-canvas__minimap"
        data-testid="flow-minimap"
        position="bottom-left"
        :width="150"
        :height="96"
        @pointerdown="onMiniDown"
        :node-color="miniNodeColor"
        :node-stroke-color="miniNodeColor"
        :node-border-radius="3"
        :mask-color="miniMaskColor"
        :mask-stroke-color="miniMaskStroke"
        :mask-stroke-width="1.5"
        zoomable
      />
    </VueFlow>

    <!-- 背景点阵之外的第二层：点阵由 <Background> 画（跟着平移缩放走），这里只补顶部品牌光晕 -->
    <div class="flow-canvas__glow" aria-hidden="true" />

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
      <button
        type="button"
        class="flow-canvas__zoom-btn flow-canvas__zoom-btn--scale"
        :title="`当前 ${zoomPercent}%，点击回到 100%`"
        data-zoom="reset"
        @click.stop="zoomTo(1)"
      >
        {{ zoomPercent }}%
      </button>
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
 * Vue Flow 画布基座（VUE-FLOW-1）。
 *
 * # 与老画布的关系
 *
 * `FlowCanvas.vue`（纯 SVG 自绘）**一行不改**保留，作为回退；本组件是同一份契约的
 * Vue Flow 实现，**props / emits / defineExpose 与它逐个对齐**：
 *
 * | 能力 | 老画布 | 本组件 |
 * |---|---|---|
 * | props | 13 个 | 同名同默认值（+ `autoFit` 兼容别名） |
 * | emits | 10 个 | 同名同签名 |
 * | expose | 7 个方法 | 同名同语义 |
 *
 * 契约一致性由 `tests/unit/flow-canvas-vueflow-contract.test.ts` 直接比对两个源文件钉住 ——
 * 任何一边加字段而另一边没跟上，单测立刻红。
 *
 * # 为什么用 Vue Flow 而不是自绘
 *
 * 自绘画布把「渲染」和「几何」揉在一个 1700 行组件里，平移缩放要自己算矩阵，Safari 上
 * 还要绕 foreignObject 的绘制 bug（见 `docs/evidence/20261104-safari-flow/`）。
 * Vue Flow 的节点是普通 div + CSS transform，天然没有 foreignObject 那一堆坑；
 * 分组、框选、对齐线、连接校验也都是现成的（VUE-FLOW-2/3 直接用）。
 *
 * # 坐标口径没有变
 *
 * `node.x/y` 仍是**画布逻辑坐标**（落库就是它），连线起终点仍按 `graph.ts` 的
 * `inPort/outPort` 常量尺寸算（见 `edgeViewOf`）。Vue Flow 只负责「怎么显示」，
 * 不参与「图怎么存」。
 *
 * # 交互分工（哪些是 Vue Flow 自带，哪些是自己接的）
 *
 * - 自带：平移（`panOnDrag`）、滚轮缩放（`zoomOnScroll`，d3-zoom 以指针为锚点）、
 *   拖节点（`nodesDraggable`）、拉线（Handle + `connect` 事件）、小地图拖拽/缩放。
 * - 自己接：选中（`node-click` / `edge-click` / `pane-click`）、拖拽落点吸附 10px 网格后
 *   发 `move-node`、键盘（Delete / Ctrl+Z / 方向键 / 空格平移）、工具栏与缩放百分比、
 *   连线落点高亮（`connect-target`）、fitView / centerOn / screenToCanvas 暴露给页面。
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import {
  ConnectionLineType,
  Handle,
  MarkerType,
  Position,
  VueFlow,
  useVueFlow,
  type ConnectionLineOptions,
  type Edge as VfEdge,
  type Node as VfNode,
} from '@vue-flow/core';
import { Background } from '@vue-flow/background';
import { MiniMap } from '@vue-flow/minimap';
import '@vue-flow/core/dist/style.css';
import '@vue-flow/core/dist/theme-default.css';
import type { FlowEdge, FlowNode } from '@/api/business/flow-api';
import { GRID, MAX_SCALE, MIN_SCALE, edgeLabelPoint, edgePath, hitTest, resolveConnectDrop, type Point } from '@/views/business/flow/graph';
import { labelTextWidth, roundedEdgePath } from '@/views/business/flow/components/rounded-path';
import { buildNodeView, isEdgeTaken, nodeSizeOf, type FlowNodeViewData } from '@/views/business/flow/components/flow-node-view';

/** 组件入参（**与 `FlowCanvas.vue` 逐个同名同默认值**，调用方一行不用改） */
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
    /** 挂载后自动适应画布（老画布叫 `fitOnMount`，这里同名；`autoFit` 是兼容别名） */
    fitOnMount?: boolean;
    /** `fitOnMount` 的兼容别名（部分调用方 / 文档按这个叫法写） */
    autoFit?: boolean;
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
    autoFit: undefined,
    canUndoLayout: false,
  },
);

/** 组件事件（**与 `FlowCanvas.vue` 逐字同签名**） */
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

/**
 * 视图状态（缩放百分比 / 小地图遮罩色）。
 *
 * `useVueFlow()` 在**渲染 `<VueFlow>` 的同一个组件**里调用：store 建在当前实例作用域并
 * provide 给子组件，因此这里的 actions 与画布内部是**同一个 store**（不是第二份状态）。
 */
const { fitView: fitViewAction, zoomTo: zoomToAction, setViewport, setCenter, getViewport, screenToFlowCoordinate, onInit } = useVueFlow();

/** 当前视口（`viewport-change` 同步；工具栏百分比用它） */
const viewportState = ref({ x: 40, y: 40, zoom: 1 });

/** 缩放百分比（缩放控件显示） */
const zoomPercent = computed(() => Math.round(viewportState.value.zoom * 100));

/** 画布是否就绪（store 初始化完成前，命令式 API 是空实现） */
const ready = ref(false);

/** 空格键是否按下（按住空格 + 拖拽 = 平移，与老画布一致） */
const spaceDown = ref(false);

/** 是否正在拉线（拉线时把节点抬到最上层，避免被别的节点挡住落点） */
const isConnecting = ref(false);

/** 连线落点命中的目标节点（只做视觉提示，规则仍由父组件判） */
const connectTargetKey = ref<string | null>(null);

/** 起点 key（拉线时排除自身，与老画布 `findDropTarget` 同口径） */
let connectFromKey: string | null = null;

/** 最近一次「按下 → 抬起」之间指针是否移动过（区分点空白取消选中 / 拖拽平移） */
let pointerMoved = false;
/** 最近一次按下位置 */
let pointerDownAt: Point = { x: 0, y: 0 };

/** 校验命中集合 */
const issueSet = computed(() => new Set(props.issueNodeKeys ?? []));

/** 已走过的边集合 */
const takenSet = computed(() => new Set(props.takenPairs ?? []));

/** 是否已用 Esc 放弃当前拉线（松手时不发 `connect`） */
const connectCancelled = ref(false);

/**
 * 本次手势里 Vue Flow 是否已经报过一条连线。
 *
 * 用来区分「Vue Flow 自己认了」和「Vue Flow 没认（松手点不在句柄 connectionRadius 内）」——
 * 后者由 `onConnectEnd` 按老画布口径兜底（见 `resolveConnectDrop`）。
 */
let connectedInGesture = false;

/** 拉线中的虚线样式（与老画布 `.flow-canvas__draft` 同款）。
 *
 * Esc 取消拉线后置为透明：Vue Flow 1.48 没有公开的 `cancelConnection`，
 * 拿不到「中断内部连接态」的 API，但可以把跟随鼠标的虚线**视觉上收掉**，
 * 同时在 `onConnect` 里吞掉这次 emit —— 用户看到的就是「Esc = 放弃这条线」。
 */
const connectionLineOptions = computed<ConnectionLineOptions>(() => ({
  type: ConnectionLineType.Straight,
  style: connectCancelled.value
    ? { stroke: 'transparent', strokeWidth: 0 }
    : { stroke: 'var(--td-brand-color)', strokeWidth: 2, strokeDasharray: '6 4' },
}));

/** 校验角标悬停提示（画布只拿到 key，规则文案在调用方的校验清单里） */
const ISSUE_HINT = '该节点有校验问题：详见画布下方的「校验未通过」清单';

/** 小地图配色（老画布：灰色块 + 品牌色视野框） */
const miniNodeColor = 'var(--td-gray-color-4)';
const miniMaskColor = 'color-mix(in srgb, var(--td-brand-color) 10%, transparent)';
const miniMaskStroke = 'var(--td-brand-color)';

/** 小地图拖拽中的宿主元素（面板 div） */
let miniHost: HTMLElement | null = null;

/**
 * 小地图按下：把点到的画布位置挪到视野中心，随后按住拖动持续跟随（老画布同款手感）。
 *
 * # 为什么不用 `<MiniMap pannable>`
 *
 * 组件内部的平移实现读 `event.sourceEvent.movementX/movementY`，而 **Safari/WebKit 没有
 * 这两个属性**（`undefined`）→ 计算出的位移是 NaN，拖小地图完全没反应（Chromium 正常）。
 * 自己算的好处还多一条：交互语义与老画布一致（点一下就跳过去），而不是「拖多少走多少」。
 */
function onMiniDown(event: PointerEvent): void {
  if (event.button !== 0) {
    return;
  }
  const host = event.currentTarget as HTMLElement | null;
  if (!host) {
    return;
  }
  event.preventDefault();
  event.stopPropagation();
  miniHost = host;
  miniCenterOn(event.clientX, event.clientY, host);
  window.addEventListener('pointermove', onMiniMove);
  window.addEventListener('pointerup', onMiniUp);
  window.addEventListener('pointercancel', onMiniUp);
}

/** 小地图拖拽：持续把指针位置对应的画布点挪到视野中心 */
function onMiniMove(event: PointerEvent): void {
  if (!miniHost) {
    return;
  }
  miniCenterOn(event.clientX, event.clientY, miniHost);
}

/** 小地图松手：摘监听 */
function onMiniUp(): void {
  window.removeEventListener('pointermove', onMiniMove);
  window.removeEventListener('pointerup', onMiniUp);
  window.removeEventListener('pointercancel', onMiniUp);
  miniHost = null;
}

/**
 * 把屏幕点换算成「小地图里的画布坐标」并居中。
 *
 * 小地图的 svg 用 `viewBox` 描述「节点包围盒 ∪ 视野框」这块区域，
 * 所以：`画布坐标 = viewBox.x + (指针在盒子内的比例 × viewBox.width)`。
 */
function miniCenterOn(clientX: number, clientY: number, host: HTMLElement): void {
  const svg = host.querySelector('svg');
  if (!svg) {
    return;
  }
  const rect = svg.getBoundingClientRect();
  if (rect.width === 0 || rect.height === 0) {
    return;
  }
  const box = svg.viewBox.baseVal;
  const point = {
    x: box.x + ((clientX - rect.left) / rect.width) * box.width,
    y: box.y + ((clientY - rect.top) / rect.height) * box.height,
  };
  void setCenter(point.x, point.y, { zoom: getViewport().zoom, duration: 0 });
}

/** Vue Flow 节点数组（由 props 派生，拖拽期间由 Vue Flow 自己维护） */
const vfNodes = ref<VfNode<FlowNodeViewData>[]>([]);
/** Vue Flow 连线数组（由 props 派生） */
const vfEdges = ref<VfEdge[]>([]);

/**
 * 节点坐标缓存：`key → props 里的权威坐标`。
 *
 * 拖拽结束时要用它算「本次拖了多远」——Vue Flow 已经把节点挪到落点了，
 * 但它挪的是内部坐标；落库口径必须是「props 原坐标 + 吸附后的偏移」（老画布同款算法），
 * 否则每次拖拽都会把 dagre 排出来的非 10 倍数坐标悄悄抹平。
 */
const positionCache = new Map<string, Point>();

/** 建一个 Vue Flow 节点 */
function toVfNode(node: FlowNode): VfNode<FlowNodeViewData> {
  const data = buildNodeView(node, {
    selectedKey: props.selectedKey,
    issueKeys: issueSet.value,
    status: props.nodeStatus,
    connectTargetKey: connectTargetKey.value,
  });
  return {
    id: node.key,
    type: 'flow',
    position: { x: node.x, y: node.y },
    // 预先给出宽高：Vue Flow 首次 fitView 前就能算出包围盒（否则要等 DOM 测量，首帧会闪）
    width: data.w,
    height: data.h,
    /**
     * ★ 锚点方向的**兜底声明**（VUE-FLOW-2 真机 bug 的根治项）。
     *
     * Vue Flow 的 `getHandlePosition()` 优先用 Handle 的 `position`，**Handle 不存在时**才回落到
     * 节点的 `sourcePosition` / `targetPosition`，而这两个的默认值是 `Bottom` / `Top`
     * （内置 DefaultNode 的默认值）。我们的节点是自定义 slot（`#node-flow`），不写这两个字段
     * 就等于「默认上下」—— 一旦某个节点没渲染 Handle（例如历史上只读态用 v-if 摘掉），
     * 连线立刻变成「底部出、顶部进」，而老画布始终是右出左进。
     *
     * 所以两条腿一起走：
     * 1. 编辑态 / 只读态都渲染 Handle（只读态 `connectable=false` + `opacity: 0`），走 Handle 的路；
     * 2. 节点上把方向声明死（right / left），Handle 因任何原因缺失都不会退化。
     */
    sourcePosition: Position.Right,
    targetPosition: Position.Left,
    data,
    draggable: !props.readonly,
    connectable: !props.readonly,
    selectable: false,
    focusable: false,
  };
}

/** 用 props 重建节点数组（坐标 / 选中 / 状态 / 校验角标全部刷新） */
function rebuildNodes(): void {
  const list = props.nodes ?? [];
  positionCache.clear();
  for (const node of list) {
    positionCache.set(node.key, { x: node.x, y: node.y });
  }
  vfNodes.value = list.map(toVfNode);
}

/** 连线视图（模板里渲染用；也可单测） */
interface EdgeView {
  /** 路径 */
  d: string;
  /** 标签文案（空串 = 不画） */
  label: string;
  /** 标签中点 x */
  labelX: number;
  /** 标签中点 y */
  labelY: number;
  /** 标签底框宽度（按字符估，与老画布同一函数） */
  labelWidth: number;
  /** 是否走过 */
  isTaken: boolean;
  /** class 列表 */
  cls: Record<string, boolean>;
}

/** 用 props 重建连线数组（端点缺失的脏边整条跳过，与老画布一致） */
function rebuildEdges(): void {
  const byKey = new Map((props.nodes ?? []).map((node) => [node.key, node]));
  const statusOf = props.nodeStatus ?? {};
  const out: VfEdge[] = [];
  for (const edge of props.edges ?? []) {
    const fromNode = byKey.get(edge.from);
    const toNode = byKey.get(edge.to);
    if (!fromNode || !toNode) {
      continue;
    }
    const label = edge.is_default ? '其他情况' : (edge.label ?? '');
    const taken = isEdgeTaken(takenSet.value, edge.from, edge.to);
    out.push({
      id: edge.key,
      source: edge.from,
      target: edge.to,
      type: 'flow',
      // 标签 / 状态都塞进 data，由自定义 edge 模板渲染（Vue Flow 只负责定位两端锚点）
      data: {
        label,
        isTaken: taken,
        isDefault: Boolean(edge.is_default),
        isSelected: props.selectedEdgeKey === edge.key,
        isFlowing: statusOf[edge.to] === 'ACTIVE',
      },
      // markerEnd 只是让 Vue Flow 知道「这条边有箭头」（自定义 edge 模板自己用 url(#…) 画）
      markerEnd: MarkerType.ArrowClosed,
      selectable: false,
      focusable: false,
    });
  }
  vfEdges.value = out;
}

/**
 * 自定义 edge 模板用：把边两端锚点算成「正交圆角折线 + 标签」。
 *
 * 锚点口径来自 `graph.ts`：起点是源节点**右边中点**、终点是目标节点**左边中点**
 * （`outPort` / `inPort`），与老画布逐像素一致。这里直接用 Vue Flow 给的
 * `sourceX/sourceY/targetX/targetY`（Handle 就落在这两个点上），省一次查表。
 */
function edgeViewOf(propsIn: { sourceX: number; sourceY: number; targetX: number; targetY: number; data?: Record<string, unknown> }): EdgeView {
  const data = (propsIn.data ?? {}) as {
    label?: string;
    isTaken?: boolean;
    isDefault?: boolean;
    isSelected?: boolean;
    isFlowing?: boolean;
  };
  const from = { x: propsIn.sourceX, y: propsIn.sourceY };
  const to = { x: propsIn.targetX, y: propsIn.targetY };
  const label = data.label ?? '';
  const point = edgeLabelPoint(from, to);
  const taken = Boolean(data.isTaken);
  return {
    // 正交折线 → 圆角折线：只换画法，点序列仍由 graph.ts 算
    d: roundedEdgePath(edgePath(from, to)),
    label,
    labelX: point.x,
    labelY: point.y,
    labelWidth: labelTextWidth(label),
    isTaken: taken,
    cls: {
      'is-selected': Boolean(data.isSelected),
      'is-default': Boolean(data.isDefault),
      'is-taken': taken,
      // 指向「进行中」节点的边走流动虚线（进度图里一眼看出当前走到哪）
      'is-flowing': Boolean(data.isFlowing),
    },
  };
}

// ==================== props → 画布 ====================

watch(() => props.nodes, () => rebuildNodes(), { deep: true });
watch(() => props.edges, () => rebuildEdges(), { deep: true });
watch(
  () => [props.selectedKey, props.selectedEdgeKey, props.nodeStatus, props.takenPairs, props.issueNodeKeys, props.readonly] as const,
  () => {
    rebuildNodes();
    rebuildEdges();
  },
  { deep: true },
);

/** 连线落点高亮（拉线时命中哪个节点） */
watch(connectTargetKey, () => {
  // 只改这一处数据，避免整表重建把拖拽中的临时坐标冲掉
  for (const node of vfNodes.value) {
    if (node.data) {
      node.data.cls['is-connect-target'] = node.id === connectTargetKey.value;
    }
  }
});

// ==================== 画布事件 ====================

/** store 就绪 */
onInit(() => {
  ready.value = true;
  viewportState.value = { ...getViewport() };
  if (shouldFitOnMount()) {
    // 等一帧拿到真实容器尺寸（弹窗 / drawer 里挂载时首帧宽高可能是 0）
    requestAnimationFrame(() => void fitView());
  }
});

/** 是否已经自动 fitView 过（`nodesInitialized` 可能触发多次，只认第一次） */
let fittedOnce = false;

/**
 * 节点尺寸测量完成 → 自动 fitView。
 *
 * 为什么不能只靠 `onInit` 里的那一帧：Vue Flow 的 fitView 依赖**已测量的节点尺寸**，
 * 挂载那一帧往往是 0（异步组件 + 图表数据后到），此时 fit 会静默不生效，
 * 画布停在默认视口（100% / 原点）——实测表现为「有时 fit 有时不 fit」。
 * `nodesInitialized` 是「尺寸就绪」的准确信号。
 */
function onNodesInitialized(): void {
  if (fittedOnce || !shouldFitOnMount()) {
    return;
  }
  fittedOnce = true;
  void fitView();
}

/** 是否挂载后 fitView（`fitOnMount` 优先，`autoFit` 为兼容别名） */
function shouldFitOnMount(): boolean {
  if (props.fitOnMount === false) {
    return false;
  }
  if (props.autoFit === false) {
    return false;
  }
  return props.fitOnMount || Boolean(props.autoFit);
}

/** 视口变化（缩放百分比 / 小地图遮罩跟着走） */
function onViewportChange(viewport: { x: number; y: number; zoom: number }): void {
  viewportState.value = { ...viewport };
}

/** 点节点：选中（只读也允许选中，与老画布一致） */
function onNodeClick(payload: { node: VfNode }): void {
  wrapRef.value?.focus();
  emit('select-node', payload.node.id);
}

/** 点连线：选中 */
function onEdgeClick(payload: { edge: VfEdge }): void {
  wrapRef.value?.focus();
  emit('select-edge', payload.edge.id);
}

/** 点空白：取消选中（只有「没拖动过」才算点，拖拽平移不取消） */
function onPaneClick(): void {
  wrapRef.value?.focus();
  if (pointerMoved) {
    return;
  }
  emit('select-node', null);
  emit('select-edge', null);
}

/**
 * 拖节点结束：吸附 10px 网格后发 `move-node`。
 *
 * 与老画布的算法逐句等价：`新坐标 = max(0, props 原坐标 + snapGrid(位移))`。
 * 注意是**吸附位移**不是吸附坐标 —— dagre 排出来的坐标不一定在网格上，
 * 吸附绝对坐标会让节点在第一次拖拽时整体跳一下。
 */
function onNodeDragStop(payload: { node: VfNode }): void {
  const { node } = payload;
  const origin = positionCache.get(node.id);
  if (!origin) {
    return;
  }
  const dx = node.position.x - origin.x;
  const dy = node.position.y - origin.y;
  const snapped: Point = { x: Math.round(dx / GRID) * GRID, y: Math.round(dy / GRID) * GRID };
  if (snapped.x === 0 && snapped.y === 0) {
    // 位移不足半格：把节点放回原处（否则会停在半格上，看起来「拖了没生效」）
    node.position = { x: origin.x, y: origin.y };
    return;
  }
  const next = { x: Math.max(0, origin.x + snapped.x), y: Math.max(0, origin.y + snapped.y) };
  node.position = next;
  emit('move-node', { key: node.id, x: next.x, y: next.y });
}

/** 拉线开始：记起点、进入连线态 */
function onConnectStart(payload: { nodeId?: string | null }): void {
  connectFromKey = payload.nodeId ?? null;
  connectTargetKey.value = null;
  connectCancelled.value = false;
  connectedInGesture = false;
  isConnecting.value = true;
  window.addEventListener('pointermove', onConnectMove);
}

/** 拉线中的全局 pointermove：命中谁就把谁标成落点目标 */
function onConnectMove(event: PointerEvent | MouseEvent): void {
  const point = screenToCanvas(event.clientX, event.clientY);
  const hit = hitTest(props.nodes ?? [], point, connectFromKey ?? undefined);
  const next = hit?.key ?? (connectFromKey && hitSelf(props.nodes ?? [], point, connectFromKey) ? connectFromKey : null);
  if (next !== connectTargetKey.value) {
    connectTargetKey.value = next;
  }
}

/** 落点是否压在起点自己身上（老画布把这种情形交回页面提示「不能连到自己」） */
function hitSelf(nodes: FlowNode[], point: Point, key: string): boolean {
  const self = nodes.find((node) => node.key === key);
  if (!self) {
    return false;
  }
  const size = nodeSizeOf(self.type);
  return point.x >= self.x && point.x <= self.x + size.w && point.y >= self.y && point.y <= self.y + size.h;
}

/** 拉线结束：清态（是否真正成边由 Vue Flow 的 connect 事件决定） */
function onConnectEnd(event?: MouseEvent): void {
  const from = connectFromKey;
  const cancelled = connectCancelled.value;
  const settledByVueFlow = connectedInGesture;
  window.removeEventListener('pointermove', onConnectMove);
  connectFromKey = null;
  connectTargetKey.value = null;
  isConnecting.value = false;
  connectedInGesture = false;

  // ★ 落点兜底（VUE-FLOW-2B 的真机 bug 根因）：
  //   Vue Flow 只认「松手点在目标 Handle 的 connectionRadius（默认 20 画布 px）内」；
  //   而本画布的落点高亮（connectTargetKey，见 onConnectMove）用的是**整张卡片**
  //   —— 与老画布口径一致。于是用户在目标卡片上松手时，卡片明明亮着绿色虚线「可以落点」，
  //   边却建不出来（Vue Flow 静默 return，连提示都没有）。
  //   这里在「Vue Flow 没认 + 没被 Esc 取消」时按老画布同一套规则再判一次：
  //   压在卡片上 → 发 connect，由父组件继续判自环 / 重复边（父组件本来就有这两个拦截）。
  if (!cancelled && !settledByVueFlow && from && event) {
    const point = screenToCanvas(event.clientX, event.clientY);
    const hit = resolveConnectDrop(props.nodes ?? [], point, from);
    if (hit) {
      emit('connect', { from, to: hit.key });
    }
  }

  // Vue Flow 的 applyDefault 会先往内部连线上塞一条；父组件拒绝时 props 不变，
  // 这里重建一次把「没被确认的边」擦掉（父组件接受时重建结果相同，幂等）。
  void nextTick(() => {
    rebuildEdges();
    connectCancelled.value = false;
  });
}

/** 拉线成功：交给父组件校验（自环 / 重复边） */
function onConnect(payload: { source?: string | null; target?: string | null }): void {
  // Vue Flow 认了这条线：本次手势不再走 connectEnd 的兜底判定
  connectedInGesture = true;
  const from = payload.source ?? '';
  const to = payload.target ?? '';
  if (!from || !to) {
    return;
  }
  // Esc 已经放弃这条线：不 emit（父组件不知道这条边存在过，等于没画）
  if (connectCancelled.value) {
    return;
  }
  emit('connect', { from, to });
}

// ==================== 指针 / 键盘 ====================

/** 记录按下点：用来区分「点空白」与「拖拽平移」 */
function onWrapPointerDown(event: PointerEvent): void {
  wrapRef.value?.focus();
  pointerMoved = false;
  pointerDownAt = { x: event.clientX, y: event.clientY };
  window.addEventListener('pointermove', onWrapPointerMove);
  window.addEventListener('pointerup', onWrapPointerUp, { once: true });
}

/** 指针移动超过 3px 就算拖拽（与老画布同阈值） */
function onWrapPointerMove(event: PointerEvent): void {
  if (Math.abs(event.clientX - pointerDownAt.x) > 3 || Math.abs(event.clientY - pointerDownAt.y) > 3) {
    pointerMoved = true;
  }
}

/** 抬起：清监听 */
function onWrapPointerUp(): void {
  window.removeEventListener('pointermove', onWrapPointerMove);
}

/** 键盘：Delete 删除 / Ctrl+Z 撤销 / 方向键微调 / 空格平移（与老画布同表） */
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
  // Esc：放弃正在拉的线（老画布没有这条，属 VUE-FLOW-2 补齐的键盘交互）；
  // 再取消选中（与「点空白」同一个出口：select-node(null) + select-edge(null)）
  if (event.key === 'Escape') {
    if (isConnecting.value) {
      event.preventDefault();
      connectCancelled.value = true;
      isConnecting.value = false;
      connectTargetKey.value = null;
    }
    if (props.selectedKey != null || props.selectedEdgeKey != null) {
      event.preventDefault();
      emit('select-node', null);
      emit('select-edge', null);
    }
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

// ==================== 命令式 API（供父组件 ref 调用，与老画布同名同语义） ====================

/**
 * 屏幕坐标 → 画布坐标（左侧面板拖入落点用）。
 *
 * Vue Flow 的 `screenToFlowCoordinate` 已经扣掉了画布容器偏移与视口变换，
 * 与老画布 `(client - rect - translate) / scale` 完全等价。
 */
function screenToCanvas(clientX: number, clientY: number): Point {
  return screenToFlowCoordinate({ x: clientX, y: clientY });
}

/** 适应画布：把所有节点装进视口（留 60 px 边距，与老画布同口径） */
async function fitView(): Promise<void> {
  if (!ready.value) {
    return;
  }
  await fitViewAction({ padding: '60px', duration: 0, minZoom: MIN_SCALE, maxZoom: MAX_SCALE });
}

/** 把某个节点居中（校验问题列表点击定位） */
function centerOn(key: string): void {
  const node = (props.nodes ?? []).find((item) => item.key === key);
  if (!node || !ready.value) {
    return;
  }
  const size = nodeSizeOf(node.type);
  const zoom = Math.max(getViewport().zoom, 0.8);
  void setCenter(node.x + size.w / 2, node.y + size.h / 2, { zoom, duration: 0 });
}

/** 重置视图（与老画布同一初始视口：40,40 / 100%） */
function resetView(): void {
  void setViewport({ x: 40, y: 40, zoom: 1 }, { duration: 0 });
}

/** 相对缩放（保持视口中心为锚点） */
function zoomBy(factor: number): void {
  const next = Math.min(MAX_SCALE, Math.max(MIN_SCALE, getViewport().zoom * factor));
  void zoomToAction(next, { duration: 0 });
}

/** 缩放到指定比例（保持视口中心为锚点） */
function zoomTo(scale: number): void {
  const next = Math.min(MAX_SCALE, Math.max(MIN_SCALE, scale));
  void zoomToAction(next, { duration: 0 });
}

/** 当前缩放比例 */
function getScale(): number {
  return getViewport().zoom;
}

onMounted(() => {
  window.addEventListener('keyup', onWindowKeyup);
  rebuildNodes();
  rebuildEdges();
});

onBeforeUnmount(() => {
  window.removeEventListener('keyup', onWindowKeyup);
  window.removeEventListener('pointermove', onWrapPointerMove);
  window.removeEventListener('pointerup', onWrapPointerUp);
  window.removeEventListener('pointermove', onConnectMove);
  window.removeEventListener('pointermove', onMiniMove);
  window.removeEventListener('pointerup', onMiniUp);
  window.removeEventListener('pointercancel', onMiniUp);
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
  getScale,
});
</script>

<style scoped lang="less">
/**
 * 视觉规范：与老画布**同一套值**（11px 圆角 / 3.5px 色条 / 30×30 图标底块 /
 * `--td-shadow-1` 投影 / 13px+11.5px 字级）。颜色一律走主题变量，22 套配色自动跟随。
 */
.flow-canvas {
  position: relative;
  width: 100%;
  height: 100%;
  overflow: hidden;
  outline: none;
  cursor: default;
  background: var(--td-bg-color-page);
  border: 1px solid var(--td-component-stroke);
  border-radius: 6px;
  // ★ 画布内不允许选中文本（VUE-FLOW-2B 真机 bug）：
  //   老画布把 `user-select: none` 写在内部的 `<svg>` 上，新画布的根节点漏了这条 ——
  //   WebKit 下「在画布上拖一次」会顺手选中提示文案/节点文字，**下一次拖拽就退化成
  //   「拖动选中的文本」**：指针事件几乎全部消失（实测 19 次 pointermove 只剩 2 次）、
  //   mouseup 收不到 → 连线状态卡死（`.vue-flow__connection` 残留、is-connecting 不清），
  //   用户看到的就是「拖不出连线」。Chromium 下不触发，所以只在 Safari 侧暴露。
  user-select: none;
  -webkit-user-select: none;
  -webkit-user-drag: none;
  touch-action: none;

  &.is-readonly {
    cursor: default;
  }

  // 空格按下的临时平移态：节点让位给底层 pane（老画布是「空格 + 在节点上按下 = 平移」）
  &.is-space-pan {
    cursor: grabbing;

    :deep(.vue-flow__node) {
      pointer-events: none;
    }
  }

  // 0×0 的 svg：只放 marker 定义
  &__defs {
    position: absolute;
    width: 0;
    height: 0;
    overflow: hidden;
  }

  &__arrow {
    fill: var(--td-text-color-placeholder);

    &--taken {
      fill: var(--td-brand-color);
    }
  }

  // Vue Flow 根节点：背景交给容器（这样顶部光晕透得上来）
  &__flow {
    position: relative;
    z-index: 1;
    width: 100%;
    height: 100%;
    background: transparent;
  }

  // 顶部柔和品牌光晕（老画布 ::before 的同款观感；Vue Flow 的层级比它高，所以单独一层）
  &__glow {
    position: absolute;
    inset: 0;
    z-index: 0;
    pointer-events: none;
    background: radial-gradient(
      ellipse 60% 45% at 50% 0%,
      color-mix(in srgb, var(--td-brand-color) 10%, transparent),
      transparent 70%
    );
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

  // ---------- 小地图（左下角）：尺寸 / 圆角 / 阴影与老画布一致 ----------
  :deep(.vue-flow__minimap.flow-canvas__minimap) {
    width: 150px;
    height: 96px;
    margin: 16px;
    overflow: hidden;
    cursor: pointer;
    background: var(--td-bg-color-container);
    border: 1px solid var(--td-component-stroke);
    border-radius: 9px;
    box-shadow: var(--td-shadow-1);
    opacity: 0.94;
    transition: opacity 0.16s ease;

    // 内部 svg 必须铺满盒子（组件默认 200×150，不铺满会「内容比框大」，
    // 而且小地图的平移换算按这个尺寸算，尺寸不一致会让拖动位移对不上）
    svg {
      display: block;
      width: 100%;
      height: 100%;
    }

    &:hover {
      opacity: 1;
    }
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

  // ---------- Vue Flow 内部元素的对齐修正 ----------
  :deep(.vue-flow__pane) {
    cursor: default;
  }

  :deep(.vue-flow__node) {
    cursor: grab;
  }

  :deep(.vue-flow__node.dragging) {
    cursor: grabbing;
  }

  // 节点句柄：与老画布的锚点圆点同款（白底 + 品牌色描边，半径 5px，描边 1.5px）。
  // Handle 盒子本身就是「锚点区域」：11.5×11.5（= 2×5 + 1.5 描边外扩），Vue Flow 用
  // `transform: translate(±50%, -50%)` 把它居中到卡片左/右边缘，盒心正好落在老画布的锚点坐标上。
  // ★ 圆点用内嵌 SVG circle 画（见模板注释）：div 的 1.5px border 会被 Chrome 取整成 1px。
  :deep(.vue-flow__handle.flow-node__port) {
    display: grid;
    place-items: center;
    box-sizing: border-box;
    width: 11.5px;
    height: 11.5px;
    min-width: 11.5px;
    min-height: 11.5px;
    padding: 0;
    background: transparent;
    border: none;
    border-radius: 50%;
    cursor: crosshair;
    transition: scale 0.12s ease;
  }

  // hover：放大 + 转成功色（老画布只有颜色变化，放大是卡面 ③ 要的「hover 放大」）
  // `scale` 是独立于 transform 的属性，不会覆盖 Vue Flow 用来定位句柄的 translate
  :deep(.vue-flow__handle.flow-node__port:hover) {
    scale: 1.25;
  }

  // ★ 起手命中区（VUE-FLOW-2B）：视觉圆点仍是 11.5×11.5，但**可点击区域**放大到
  //   11.5 + 7×2 = 25.5 CSS px（zoom 100% 时 25.5px，fitView 到 53% 时也有 ~13.5px 屏上直径）。
  //   改前命中区等于视觉圆点本身（屏上 ~6px），真鼠标极易点偏 —— 点偏后事件落到卡片上，
  //   变成「拖节点」而不是「拉线」，连预览线都不会出现（用户原话：看到圆点但拖不出线）。
  //   ★ 用 ::before 而不是给元素加 padding/width：`getHandleBounds` 读的是元素 rect，
  //     元素盒一旦变大，连线端点坐标就会跟着漂（老画布圆点在卡片边缘上，端点多出半格）。
  //     伪元素不参与 rect，视觉与端点坐标都不动。
  :deep(.vue-flow__handle.flow-node__port)::before {
    content: '';
    position: absolute;
    inset: -7px;
    border-radius: 50%;
  }

  // 只读：**元素必须留在 DOM 里**（否则 Vue Flow 拿不到锚点坐标，连线会退化成底部出/顶部进），
  // 只用 opacity 做视觉隐藏；绝不能 display:none / visibility:hidden / width:0。
  :deep(.vue-flow__handle.flow-node__port.is-readonly) {
    opacity: 0;
    pointer-events: none;
    cursor: default;
    scale: 1;
  }

  // 边：视觉线不吃指针，命中靠 14px 透明描边（老画布同款分工）
  :deep(.vue-flow__edge) {
    cursor: pointer;
  }
}

// ---------- 节点卡片（与老画布逐值一致） ----------
.flow-node {
  &__card {
    box-sizing: border-box;
    position: relative;
    display: flex;
    gap: 8px;
    align-items: center;
    padding: 11px 13px;
    overflow: hidden;
    cursor: grab;
    background: var(--td-bg-color-container);
    border: 1px solid var(--td-component-stroke);
    border-radius: 11px;
    box-shadow: var(--td-shadow-1);
    user-select: none;
    transition:
      box-shadow 0.18s ease,
      transform 0.18s ease,
      border-color 0.18s ease;
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

  // 校验角标（胶囊右上角外侧的红 ! 圆点）
  //
  // 几何与老画布 `<circle cx="w-4" cy="4" r="8">` 等价：16×16 的圆，右边/上边各外扩 4px。
  // 它是**卡片的兄弟节点**（不是子节点）：卡片有 overflow: hidden，放里面会被裁成半圆。
  // 定位基准是 Vue Flow 的节点盒子（与卡片同尺寸），所以 right/top 直接用负偏移。
  &__issue {
    position: absolute;
    top: -4px;
    right: -4px;
    z-index: 2;
    display: grid;
    place-items: center;
    box-sizing: border-box;
    width: 16px;
    height: 16px;
    font-size: 11px;
    font-weight: 700;
    line-height: 1;
    color: var(--td-text-color-anti);
    cursor: help;
    background: var(--td-error-color);
    border-radius: 50%;
  }

  // 开始 / 结束（高 44）放不下两行，切紧凑排版
  &__card.is-compact {
    gap: 7px;
    padding: 5px 10px;

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

  // ---------- 九种节点的类型色（全部走主题变量 / 色阶组合，不写裸 hex） ----------
  &__card.is-start {
    --node-accent: var(--td-success-color);
  }

  &__card.is-end {
    --node-accent: var(--td-text-color-disabled);
  }

  &__card.is-approval {
    --node-accent: var(--td-brand-color);
  }

  // 会签：同色系更深一档（不与审批撞色）
  &__card.is-countersign {
    --node-accent: color-mix(in srgb, var(--td-brand-color) 70%, var(--td-text-color-primary));
  }

  &__card.is-condition {
    --node-accent: var(--td-warning-color);
  }

  &__card.is-parallel {
    --node-accent: color-mix(in srgb, var(--td-brand-color-7) 55%, var(--td-error-color));
  }

  // 抄送：浅色阶（只读知会，视觉上最轻）
  &__card.is-cc {
    --node-accent: var(--td-brand-color-4);
  }

  // 任务：与审批同族但更深（两者常相邻，撞色会让人以为同一类）
  &__card.is-task {
    --node-accent: color-mix(in srgb, var(--td-brand-color) 62%, var(--td-success-color));
  }

  // 通知：最轻的一档
  &__card.is-notify {
    --node-accent: color-mix(in srgb, var(--td-warning-color) 55%, var(--td-text-color-placeholder));
  }

  // 锚点圆点（内嵌 SVG：与老画布同一条 `circle r=5 / stroke-width 1.5`，栅格化结果一致）
  &__port-svg {
    display: block;
    width: 11.5px;
    height: 11.5px;
    overflow: visible;
    pointer-events: none;
  }

  &__port-dot {
    fill: var(--td-bg-color-container);
    stroke: var(--td-brand-color);
    stroke-width: 1.5;
    transition:
      fill 0.12s ease,
      stroke 0.12s ease;
  }

  // hover 出口锚点 / 拉线落点命中：转成功色（老画布 `.flow-node__port--out:hover`、`.is-target` 同款）
  &__port:hover &__port-dot,
  &__port.is-target &__port-dot {
    fill: var(--td-success-color);
    stroke: var(--td-success-color);
  }

  // ---------- 交互态 ----------
  &__card:hover {
    box-shadow: var(--td-shadow-2);
    transform: translateY(-2px);
  }

  &__card.is-selected {
    border-color: var(--node-accent, var(--td-brand-color));
    box-shadow:
      0 0 0 2px color-mix(in srgb, var(--node-accent, var(--td-brand-color)) 35%, transparent),
      var(--td-shadow-2);
  }

  &__card.has-issue {
    border-color: var(--td-error-color);
    border-style: dashed;
  }

  // ---------- 运行态（进度图） ----------
  &__card.is-approved {
    border-color: var(--td-success-color);
  }

  &__card.is-active {
    border-color: var(--td-brand-color);
  }

  &__card.is-rejected {
    border-color: var(--td-error-color);
  }

  &__card.is-skipped {
    opacity: 0.55;
  }

  // 连线拖到它头上：虚线提示可以落点
  &__card.is-connect-target {
    border-color: var(--td-success-color);
    border-style: dashed;
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--td-success-color) 30%, transparent);
  }
}

// ---------- 连线 ----------
.flow-edge {
  &__hit {
    // ★ 少了这一行就是「连线变黑块」：SVG path 默认 fill 是黑色，
    // 圆角正交 path 的贝塞尔会围出弧形填充区域。属性 fill="none" 同义，双保险。
    fill: none;
    stroke: transparent;
    stroke-width: 14;
    cursor: pointer;
    // 只吃描边不吃填充
    pointer-events: stroke;
  }

  &__line {
    fill: none;
    stroke: var(--td-gray-color-5);
    stroke-width: 1.9;
    stroke-linecap: round;
    stroke-linejoin: round;
    // 视觉线不吃指针：命中交给上面那条 14px 宽的透明命中路径
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
/** 深色主题微调（故意不 scoped：`html[theme-mode='dark']` 写在 scoped 块里永远不匹配） */
html[theme-mode='dark'] .flow-canvas__glow {
  opacity: 0.5;
}
</style>

<template>
  <component :is="activeCanvas" v-bind="passProps" v-on="passListeners" ref="inner" />
</template>

<script setup lang="ts">
/**
 * 画布切换器（feature flag 的唯一接入点）。
 *
 * # 它解决什么
 *
 * 画布有两套实现（`FlowCanvas.vue` 纯 SVG / `FlowCanvasVueFlow.vue` Vue Flow），
 * props / emits / 暴露方法逐个对齐。调用方**不需要关心**用哪套 ——
 * 只要从本组件导入（模板里的标签、所有 props / 事件 / `ref` 用法一行都不用改）：
 *
 * ```diff
 * - import FlowCanvas from '@/views/business/flow/components/FlowCanvas.vue';
 * + import FlowCanvas from '@/views/business/flow/components/FlowCanvasSwitch.vue';
 * ```
 *
 * 回退同样的道理：把这一行改回去即可（不需要发版逻辑、不需要动老画布）。
 *
 * # 为什么新画布用异步组件
 *
 * `@vue-flow/*` 合计 ~290 KiB（gzip ~100 KiB）。默认引擎已是 vueflow（VUE-FLOW-3），
 * 但异步组件保证这块代码**只进编辑器路由的 chunk，不进首屏** ——
 * `vite build` 输出可见 `FlowCanvasVueFlow-*.js` 独立成块（见 docs/evidence/20261104-vueflow-3/）。
 *
 * # flag 优先级 / 回退
 *
 * `?canvas=native|vueflow` > `VITE_FLOW_CANVAS=native|vueflow` > 默认 `vueflow`，
 * 见 `flow-canvas-flag.ts` 顶部注释（含三条回退方法）。
 */
import { computed, defineAsyncComponent, ref, type Component, type ComponentPublicInstance } from 'vue';
import NativeCanvas from '@/views/business/flow/components/FlowCanvas.vue';
import type { FlowEdge, FlowNode } from '@/api/business/flow-api';
import { resolveFlowCanvasEngine } from '@/views/business/flow/components/flow-canvas-flag';

/** 新画布：异步加载（不进首屏 chunk） */
const VueFlowCanvas = defineAsyncComponent(() => import('@/views/business/flow/components/FlowCanvasVueFlow.vue'));

/** 本次挂载用哪套画布（刷新页面时重新判定；画布不需要热切换） */
const engine = resolveFlowCanvasEngine();

/**
 * 当前生效的画布组件。
 *
 * 标成 `Component` 是故意的：两套画布 props 同名同类型，让 TS 去比对「联合类型」
 * 只会得到一堆 `ref` 之类的噪音错误；真正的契约一致性由单测（比对两个源文件）兜。
 */
const activeCanvas = computed<Component>(() => (engine === 'vueflow' ? VueFlowCanvas : NativeCanvas));

/** 内层画布实例 */
const inner = ref<ComponentPublicInstance | null>(null);

/**
 * 入参：**与 `FlowCanvas.vue` 逐个同名同默认值**（这样调用方传参与类型检查都无感）。
 *
 * 默认值必须在这里再写一份：`defineProps<{ x?: boolean }>()` 的 boolean 在运行时会被 Vue
 * 做「缺省转 false」的转换，若不显式给默认值，「调用方没传 showToolbar」就会变成
 * 「显式传 false」，把内层画布的默认值（true）盖掉（实测：工具栏 / 小地图 / 提示全消失）。
 * 两份默认值的一致性由 `tests/unit/flow-canvas-vueflow-contract.test.ts` 的 ⑪ 钉住。
 */
const props = withDefaults(
  defineProps<{
    /** 节点 */
    nodes: FlowNode[];
    /** 连线 */
    edges: FlowEdge[];
    /** 只读 */
    readonly?: boolean;
    /** 选中节点 key */
    selectedKey?: string | null;
    /** 选中连线 key */
    selectedEdgeKey?: string | null;
    /** 校验命中节点 key */
    issueNodeKeys?: string[];
    /** 缩放工具栏 */
    showToolbar?: boolean;
    /** 小地图 */
    showMinimap?: boolean;
    /** 底部提示 */
    showHint?: boolean;
    /** 节点运行状态 */
    nodeStatus?: Record<string, string>;
    /** 已走过的边 */
    takenPairs?: string[];
    /** 挂载后 fitView */
    fitOnMount?: boolean;
    /** `fitOnMount` 的兼容别名 */
    autoFit?: boolean;
    /** 刚做过自动布局 */
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

/** 事件：与 `FlowCanvas.vue` 逐字同签名 */
const emit = defineEmits<{
  (event: 'select-node', key: string | null): void;
  (event: 'select-edge', key: string | null): void;
  (event: 'move-node', payload: { key: string; x: number; y: number }): void;
  (event: 'connect', payload: { from: string; to: string }): void;
  (event: 'delete-selected'): void;
  (event: 'undo'): void;
  (event: 'redo'): void;
  (event: 'nudge', payload: { key: string; dx: number; dy: number }): void;
  (event: 'auto-layout'): void;
  (event: 'undo-layout'): void;
}>();

/** 透传的 props（`undefined` 剔除：`autoFit` 未传时不该覆盖内层的「未设置」语义） */
const passProps = computed(() => {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(props)) {
    if (value !== undefined) {
      out[key] = value;
    }
  }
  return out;
});

/** 透传的监听器（10 个事件逐个转发，`unknown` 收窄交给内层组件） */
const passListeners = {
  'select-node': (key: string | null) => emit('select-node', key),
  'select-edge': (key: string | null) => emit('select-edge', key),
  'move-node': (payload: { key: string; x: number; y: number }) => emit('move-node', payload),
  connect: (payload: { from: string; to: string }) => emit('connect', payload),
  'delete-selected': () => emit('delete-selected'),
  undo: () => emit('undo'),
  redo: () => emit('redo'),
  nudge: (payload: { key: string; dx: number; dy: number }) => emit('nudge', payload),
  'auto-layout': () => emit('auto-layout'),
  'undo-layout': () => emit('undo-layout'),
};

/** 内层画布暴露出来的命令式 API（两套实现同名同语义） */
interface CanvasApi {
  screenToCanvas(clientX: number, clientY: number): { x: number; y: number };
  fitView(): void;
  centerOn(key: string): void;
  resetView(): void;
  zoomBy(factor: number): void;
  zoomTo(scale: number): void;
  getScale(): number;
}

/** 取内层实例的 API（异步组件尚未加载完成时返回 null） */
function api(): Partial<CanvasApi> | null {
  return (inner.value as unknown as Partial<CanvasApi> | null) ?? null;
}

/**
 * 暴露的命令式 API（与 `FlowCanvas.vue` 的 `defineExpose` 逐个同名同语义）。
 *
 * 页面里用的是 `canvasRef.value?.fitView()` / `screenToCanvas()` / `centerOn()`，
 * 转发一层后调用点完全不用改。
 */
defineExpose({
  /** 屏幕坐标 → 画布坐标 */
  screenToCanvas: (clientX: number, clientY: number): { x: number; y: number } => api()?.screenToCanvas?.(clientX, clientY) ?? { x: 0, y: 0 },
  /** 适应画布 */
  fitView: (): void => api()?.fitView?.(),
  /** 定位到某节点 */
  centerOn: (key: string): void => api()?.centerOn?.(key),
  /** 重置视图 */
  resetView: (): void => api()?.resetView?.(),
  /** 相对缩放 */
  zoomBy: (factor: number): void => api()?.zoomBy?.(factor),
  /** 缩放到指定比例 */
  zoomTo: (scale: number): void => api()?.zoomTo?.(scale),
  /** 当前缩放比例 */
  getScale: (): number => api()?.getScale?.() ?? 1,
});
</script>

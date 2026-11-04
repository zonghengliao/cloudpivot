<template>
  <div class="flow-preview" :style="{ height }">
    <FlowCanvas
      :nodes="nodes"
      :edges="edges"
      readonly
      :show-toolbar="false"
      :show-hint="false"
      :node-status="nodeStatus"
      :taken-pairs="takenPairs"
      :selected-key="null"
      :selected-edge-key="null"
    />
  </div>
</template>

<script setup lang="ts">
/**
 * 只读流程图预览（发起页「流程预览」/ 详情页「流程图」共用）。
 *
 * 复用编辑器的 `FlowCanvas.vue`（同一套坐标、形状、语义色），只是强制只读：
 * - 不画锚点、不响应拖拽 / 连线 / 删除；
 * - 不带缩放工具栏与操作提示；
 * - 挂载后自动「适应画布」，小窗口里也能看全整张图。
 *
 * # 为什么不做第二个渲染器
 *
 * 流程图有两种画法（编辑器一套、预览一套）必然漂移：预览里节点的宽度和编辑器不一致，
 * 连线就会错位。这里只做一层只读包装，几何与配色**只有一份实现**。
 */
import type { FlowEdge, FlowNode } from '@/api/business/flow-api';
// 画布接入点：FlowCanvasSwitch 内部按 feature flag 选「老画布 / Vue Flow 画布」
import FlowCanvas from '@/views/business/flow/components/FlowCanvasSwitch.vue';

withDefaults(
  defineProps<{
    /** 节点 */
    nodes: FlowNode[];
    /** 连线 */
    edges: FlowEdge[];
    /** 高度（CSS 长度） */
    height?: string;
    /** 节点运行状态（`APPROVED|ACTIVE|REJECTED|SKIPPED|PENDING`） */
    nodeStatus?: Record<string, string>;
    /** 已走过的边（`${from}>${to}`） */
    takenPairs?: string[];
  }>(),
  { height: '260px', nodeStatus: () => ({}), takenPairs: () => [] },
);
</script>

<style scoped lang="less">
.flow-preview {
  width: 100%;
  overflow: hidden;
  border-radius: 6px;
}
</style>

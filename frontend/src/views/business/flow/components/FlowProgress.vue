<template>
  <div class="flow-progress">
    <template v-for="(node, index) in nodes" :key="`${node.node_key}-${node.round}`">
      <div class="flow-progress__step" :class="stepClass(node)">
        <div class="flow-progress__dot">
          <t-icon v-if="node.status === 'APPROVED'" name="check" size="14px" />
          <t-icon v-else-if="node.status === 'REJECTED'" name="close" size="14px" />
          <t-icon v-else-if="node.status === 'SKIPPED'" name="minus" size="14px" />
          <t-icon v-else-if="node.status === 'TERMINATED'" name="stop-circle" size="14px" />
          <span v-else class="flow-progress__index">{{ index + 1 }}</span>
        </div>
        <div class="flow-progress__text">
          <div class="flow-progress__name" :title="node.name">{{ node.name }}</div>
          <div class="flow-progress__meta">
            <span class="flow-progress__status">{{ statusLabel(node.status) }}</span>
            <span v-if="node.type === 'COUNTERSIGN' && node.approver_count" class="flow-progress__ratio">
              {{ node.approved_count ?? 0 }}/{{ node.approver_count }}
            </span>
            <span v-if="node.round > 1" class="flow-progress__round">第 {{ node.round }} 轮</span>
          </div>
        </div>
      </div>
      <div
        v-if="index < nodes.length - 1"
        class="flow-progress__line"
        :class="{ 'is-done': node.status === 'APPROVED' || node.status === 'SKIPPED' }"
      />
    </template>
    <div v-if="nodes.length === 0" class="flow-progress__empty">暂无节点进度</div>
  </div>
</template>

<script setup lang="ts">
/**
 * 审批进度条（卡面页面 5 的「流程进度条」）。
 *
 * 横向排列实例的节点实例，三态配色：
 * - 已过（`APPROVED` / `SKIPPED`）：绿色、实线；
 * - 当前（`ACTIVE`）：品牌色高亮 + 呼吸动画；
 * - 未到（`PENDING`）：灰色虚线。
 *
 * 数据只来自后端 `GET /business/flow/instance/{id}` 的 `nodes[]`，
 * **前端不自己推「走到第几步」**——节点状态是引擎写的，前端猜一次就可能和真实状态不一致。
 */
import { statusMeta } from '@/constants/status-map';

/** 进度条要的最小节点形状（详情页把 `NodeProgressRes` 映射成它） */
export interface FlowProgressNode {
  /** 节点 key */
  node_key: string;
  /** 节点中文名 */
  name: string;
  /** 节点类型 */
  type: string;
  /** 轮次 */
  round: number;
  /** 节点实例状态 */
  status: string;
  /** 应到人数（会签） */
  approver_count?: number;
  /** 已同意（会签） */
  approved_count?: number;
  /** 耗时（毫秒，0/缺省不显示） */
  duration_ms?: number;
}

defineProps<{
  /** 节点进度（按图顺序） */
  nodes: FlowProgressNode[];
}>();

/** 状态文案（未知状态原样显示） */
function statusLabel(status: string): string {
  return statusMeta('flow-node', status).label;
}

/** 步骤的 class（决定配色） */
function stepClass(node: FlowProgressNode): Record<string, boolean> {
  return {
    'is-done': node.status === 'APPROVED',
    'is-skipped': node.status === 'SKIPPED' || node.status === 'TERMINATED',
    'is-active': node.status === 'ACTIVE',
    'is-rejected': node.status === 'REJECTED',
    'is-pending': node.status === 'PENDING' || !node.status,
  };
}
</script>

<style scoped lang="less">
.flow-progress {
  display: flex;
  align-items: flex-start;
  padding: 8px 4px 4px;
  overflow-x: auto;

  &__step {
    display: flex;
    flex: none;
    gap: 8px;
    align-items: center;
    max-width: 220px;
    padding: 4px 6px;
    border-radius: 6px;
  }

  &__dot {
    display: flex;
    flex: none;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    color: var(--td-text-color-placeholder);
    background: var(--td-bg-color-container);
    border: 2px solid var(--td-component-border);
    border-radius: 50%;
  }

  &__index {
    font-size: 12px;
    line-height: 1;
  }

  &__text {
    min-width: 0;
  }

  &__name {
    overflow: hidden;
    font-size: 13px;
    font-weight: 500;
    color: var(--td-text-color-primary);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  &__meta {
    display: flex;
    gap: 6px;
    align-items: center;
    font-size: 11px;
    color: var(--td-text-color-placeholder);
  }

  &__ratio,
  &__round {
    color: var(--td-text-color-secondary);
  }

  &__line {
    flex: 1;
    min-width: 24px;
    height: 2px;
    margin-top: 14px;
    background: var(--td-component-stroke);

    &.is-done {
      background: var(--td-success-color);
    }
  }

  &__empty {
    padding: 12px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  // ---------- 状态配色 ----------
  &__step.is-done &__dot {
    color: var(--td-text-color-anti);
    background: var(--td-success-color);
    border-color: var(--td-success-color);
  }

  &__step.is-active &__dot {
    color: var(--td-text-color-anti);
    background: var(--td-brand-color);
    border-color: var(--td-brand-color);
    box-shadow: 0 0 0 4px var(--td-brand-color-1);
    animation: flow-progress-pulse 1.6s ease-in-out infinite;
  }

  &__step.is-active &__name {
    color: var(--td-brand-color);
  }

  &__step.is-rejected &__dot {
    color: var(--td-text-color-anti);
    background: var(--td-error-color);
    border-color: var(--td-error-color);
  }

  &__step.is-skipped {
    opacity: 0.6;
  }

  &__step.is-skipped &__dot {
    border-style: dashed;
  }
}

@keyframes flow-progress-pulse {
  0%,
  100% {
    box-shadow: 0 0 0 2px var(--td-brand-color-1);
  }

  50% {
    box-shadow: 0 0 0 6px var(--td-brand-color-2);
  }
}
</style>

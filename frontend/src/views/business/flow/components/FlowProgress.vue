<template>
  <div class="flow-progress-wrap">
    <div class="flow-progress">
      <template v-for="(node, index) in orderedNodes" :key="`${node.node_key}-${node.round}`">
        <div class="flow-progress__step" :class="stepClass(node.status)">
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
          v-if="index < orderedNodes.length - 1"
          class="flow-progress__line"
          :class="{ 'is-done': node.status === 'APPROVED' || node.status === 'SKIPPED' }"
        />
      </template>
      <div v-if="orderedNodes.length === 0" class="flow-progress__empty">暂无节点进度</div>
    </div>

    <!--
      审批人明细（可选；不传 `tasks` 的老页面渲染结果与之前一致）。
      只在传了 `tasks` 时出现：每个节点下面列出「谁 / 已批准还是待处理 / 什么时间 / 意见」。
    -->
    <div v-if="taskGroups.length > 0" class="flow-progress__tasks" data-testid="flow-progress-tasks">
      <div v-for="group in taskGroups" :key="`${group.node_key}-${group.round}`" class="flow-progress__group">
        <div class="flow-progress__group-name">
          {{ group.name }}
          <span v-if="group.round > 1" class="flow-progress__round">第 {{ group.round }} 轮</span>
        </div>
        <ul class="flow-progress__task-list">
          <li
            v-for="(task, index) in group.tasks"
            :key="`${task.assignee_id || task.assignee_name}-${index}`"
            class="flow-progress__task"
            :class="taskClass(task.status)"
          >
            <span class="flow-progress__task-dot" />
            <span class="flow-progress__task-who">{{ task.assignee_name || '未指派' }}</span>
            <span class="flow-progress__task-what">{{ taskLabel(task.status) }}</span>
            <span v-if="formatTime(task.at)" class="flow-progress__task-time">{{ formatTime(task.at) }}</span>
            <t-tag v-if="task.is_auto" size="small" variant="outline">系统代点</t-tag>
            <t-tag v-if="task.is_self_approval" size="small" variant="outline" theme="warning">自审</t-tag>
            <span v-if="task.comment" class="flow-progress__task-comment">{{ task.comment }}</span>
          </li>
        </ul>
      </div>
    </div>
  </div>
</template>

<script lang="ts">
// 老调用方一直是从这个 `.vue` 里取类型（`import FlowProgress, { type FlowProgressNode } ...`），
// 类型的**单一来源**现在在 `flow-progress-tasks.ts`（纯逻辑可单测），这里只做再导出，别删。
export type { FlowProgressNode, FlowProgressTask } from './flow-progress-tasks';
</script>

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
 *
 * 可选 `tasks`（同一接口的 `timeline[]`）：传了就在进度条下面按节点列出
 * 「审批人 + 状态 + 时间 + 意见」。**进度条本身还是这一份代码**——
 * 单据详情要「谁批的 / 几点批的 / 写了什么」时不要再另画一条时间线，
 * 把 `timeline` 映射成 `FlowProgressTask` 传进来即可。
 */
import { computed } from 'vue';

import { formatDateTime } from '@/utils/format';

import {
  groupProgressTasks,
  orderProgressNodes,
  progressNodeClass,
  progressNodeLabel,
  progressTaskClass,
  progressTaskLabel,
  type FlowProgressNode,
  type FlowProgressTask,
} from './flow-progress-tasks';

const props = defineProps<{
  /** 节点进度（按图顺序） */
  nodes: FlowProgressNode[];
  /** 审批人明细（可选；空 / 不传 = 只画进度条，与老页面一致） */
  tasks?: FlowProgressTask[];
}>();

/**
 * 显示顺序（`START` 第一、`END` 最后、轮次升序）。
 *
 * 后端节点行是按 `round, created_at, node_key` 排的，同一秒创建时会按 key 排 ——
 * 真接口里出现过「金额分级① → 部门主管审批 → 开始 → 结束」。进度条这么画等于把
 * 「开始」放到最后，所以这里统一纠一次序（口径在 `flow-progress-tasks.ts`）。
 */
const orderedNodes = computed(() => orderProgressNodes(props.nodes));

/** 分组也按同一条显示顺序排，免得明细列表和进度条顺序对不上（空节点不出组，节点表里没有的 key 也显示） */
const taskGroups = computed(() => groupProgressTasks(orderedNodes.value, props.tasks));

/** 模板里的四个别名（状态文案与配色口径都在 `flow-progress-tasks.ts`） */
const statusLabel = progressNodeLabel;
const taskLabel = progressTaskLabel;
const stepClass = progressNodeClass;
const taskClass = progressTaskClass;

/** 时间：`YYYY-MM-DD HH:mm:ss`（空 → 空串，整段就不渲染） */
function formatTime(value: string | null | undefined): string {
  if (!value) {
    return '';
  }
  const text = formatDateTime(value);
  return text === '-' ? '' : text;
}
</script>

<style scoped lang="less">
.flow-progress-wrap {
  min-width: 0;
}

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

  // ---------- 审批人明细 ----------
  &__tasks {
    margin: 4px 4px 0;
    padding-top: 4px;
    border-top: 1px dashed var(--td-component-stroke);
  }

  &__group {
    padding: 4px 0 2px;
  }

  &__group-name {
    display: flex;
    gap: 6px;
    align-items: center;
    font-size: 12px;
    font-weight: 500;
    color: var(--td-text-color-secondary);
  }

  &__task-list {
    margin: 6px 0 0;
    padding: 0 0 0 4px;
    list-style: none;
  }

  &__task {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
    padding: 3px 0;
    font-size: 12px;
    line-height: 18px;
    color: var(--td-text-color-primary);
  }

  &__task-dot {
    flex: none;
    width: 6px;
    height: 6px;
    background: var(--td-text-color-placeholder);
    border-radius: 50%;
  }

  &__task.is-approved &__task-dot {
    background: var(--td-success-color);
  }

  &__task.is-rejected &__task-dot {
    background: var(--td-error-color);
  }

  &__task.is-pending &__task-dot {
    border: 1px solid var(--td-text-color-placeholder);
    background: transparent;
  }

  &__task.is-closed {
    opacity: 0.6;
  }

  &__task-who {
    font-weight: 500;
  }

  &__task-what {
    color: var(--td-text-color-secondary);
  }

  &__task.is-approved &__task-what {
    color: var(--td-success-color);
  }

  &__task.is-rejected &__task-what {
    color: var(--td-error-color);
  }

  &__task-time {
    color: var(--td-text-color-placeholder);
  }

  &__task-comment {
    color: var(--td-text-color-secondary);

    &::before {
      content: '「';
    }

    &::after {
      content: '」';
    }
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

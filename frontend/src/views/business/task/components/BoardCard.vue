<template>
  <div
    class="board-card"
    :class="{ 'board-card--pending': pending, 'board-card--terminal': terminal }"
    :draggable="!pending"
    :title="task.title"
    @dragstart="handleDragStart"
    @dragend="handleDragEnd"
    @click="handleClick"
  >
    <!-- ① 优先级 + 任务号 -->
    <div class="board-card__head">
      <PriorityTag :priority="task.priority" />
      <span class="board-card__no">{{ task.task_no }}</span>
      <t-tag v-if="task.need_review" class="board-card__review" size="small" variant="outline" theme="default">
        需验收
      </t-tag>
    </div>

    <!-- ② 标题：2 行截断，完整标题走 title 属性（鼠标悬停可见） -->
    <p class="board-card__title">{{ task.title }}</p>

    <!-- ③ 进度条 -->
    <div class="board-card__progress">
      <t-progress :percentage="clampedProgress" :label="false" theme="line" :stroke-width="4" />
      <span class="board-card__percent">{{ clampedProgress }}%</span>
    </div>

    <!-- ④ 负责人 + 截止日期（逾期红） -->
    <div class="board-card__foot">
      <span class="board-card__owner">
        <t-icon name="user" />
        {{ task.owner_name || '—' }}
      </span>
      <span class="board-card__due" :class="{ 'board-card__due--overdue': overdue }">
        <t-icon name="calendar" />
        {{ dueText }}
      </span>
    </div>

    <!-- 拖拽中的乐观更新占位：请求未落地前禁止二次拖动 -->
    <span v-if="pending" class="board-card__spinner"><t-loading size="12px" /></span>
  </div>
</template>

<script setup lang="ts">
/**
 * 看板卡片（T7b）。
 *
 * # 字段边界
 *
 * 数据源是 `TaskCard`（`GET /business/task/page`）——**没有** `estimate_hours` /
 * `description` / `creator_name`，所以卡片不画工时（列头「总预估工时」也因此不做）。
 *
 * # 拖拽
 *
 * 原生 HTML5 `draggable`：卡片只负责「起拖 / 结束」两个事件并把任务交给列，
 * **不**自己发请求 —— 状态流转的乐观更新与回弹在 `TaskBoard.vue` 统一处理，
 * 否则「拖失败要回弹」会被拆到两个组件里各写一遍。
 *
 * # 颜色
 *
 * 逾期红 / 进度条一律走主题变量（`--td-error-color` / `--td-brand-color`），
 * 配色方案切换与暗色模式自动跟随，不写 hex。
 */
import { computed } from 'vue';
import dayjs from 'dayjs';
import PriorityTag from './PriorityTag.vue';
import type { TaskCard } from '@/api/business/task-api';

const props = defineProps<{
  /** 任务卡 */
  task: TaskCard;
  /** 该卡片正处于「乐观更新 → 等后端确认」的窗口（禁止再拖、显示 loading） */
  pending?: boolean;
}>();

const emit = defineEmits<{
  /** 起拖（把任务与原始状态交给看板） */
  (event: 'drag-start', task: TaskCard): void;
  /** 拖结束（清空看板的拖拽态） */
  (event: 'drag-end'): void;
  /** 点击卡片 → 打开详情抽屉 */
  (event: 'open', task: TaskCard): void;
}>();

/** 进度收敛到 0~100（后端理论上不会越界，防御性处理避免进度条溢出） */
const clampedProgress = computed(() => Math.min(100, Math.max(0, Math.round(props.task.progress ?? 0))));

/** 终态（已完成 / 已取消）：卡片整体降透明度，视觉上与在办任务区分 */
const terminal = computed(() => props.task.status === 'DONE' || props.task.status === 'CANCELLED');

/** 是否逾期：有截止日、早于今天、且未完成/未取消（今天按本地 CST 计算） */
const overdue = computed(() => {
  if (!props.task.due_date || terminal.value) {
    return false;
  }
  return dayjs(props.task.due_date).startOf('day').isBefore(dayjs().startOf('day'));
});

/** 截止日文案：`MM-DD`（无期限显示横线；跨年时才带年份，节省卡片横向空间） */
const dueText = computed(() => {
  const due = props.task.due_date;
  if (!due) {
    return '无期限';
  }
  const date = dayjs(due);
  return date.year() === dayjs().year() ? date.format('MM-DD') : date.format('YYYY-MM-DD');
});

/** 起拖：把任务写进 dataTransfer（跨列读取用），并通知看板记下原列 */
function handleDragStart(event: DragEvent): void {
  if (props.pending) {
    event.preventDefault();
    return;
  }
  event.dataTransfer?.setData('text/plain', props.task.id);
  if (event.dataTransfer) {
    // `move` 让光标显示「移动」而不是「复制」
    event.dataTransfer.effectAllowed = 'move';
  }
  emit('drag-start', props.task);
}

/** 拖结束（无论成功 / 失败 / 取消都会触发） */
function handleDragEnd(): void {
  emit('drag-end');
}

/** 点卡片打开详情（拖动过程中的 click 由浏览器抑制，不会误触） */
function handleClick(): void {
  emit('open', props.task);
}
</script>

<style scoped lang="less">
.board-card {
  position: relative;
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 10px 12px;
  background: var(--td-bg-color-container);
  border: 1px solid var(--td-component-stroke);
  border-radius: var(--td-radius-medium);
  cursor: grab;
  transition:
    box-shadow 0.2s,
    border-color 0.2s,
    transform 0.2s;

  &:hover {
    border-color: var(--td-brand-color);
    box-shadow: var(--td-shadow-1, 0 2px 8px rgb(0 0 0 / 8%));
  }

  &:active {
    cursor: grabbing;
  }

  &--terminal {
    opacity: 0.72;
  }

  &--pending {
    cursor: progress;
    border-style: dashed;
    border-color: var(--td-brand-color);
  }

  &__head {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }

  &__no {
    overflow: hidden;
    font-family: var(--app-font-mono, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace);
    font-size: 11px;
    color: var(--td-text-color-placeholder);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  &__review {
    flex-shrink: 0;
    margin-left: auto;
  }

  &__title {
    display: -webkit-box;
    margin: 0;
    overflow: hidden;
    font-size: 13px;
    font-weight: 500;
    line-height: 1.45;
    color: var(--td-text-color-primary);
    word-break: break-all;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
  }

  &__progress {
    display: flex;
    align-items: center;
    gap: 8px;

    :deep(.t-progress__bar) {
      flex: 1;
    }
  }

  &__percent {
    flex-shrink: 0;
    width: 32px;
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    text-align: right;
    color: var(--td-text-color-secondary);
  }

  &__foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    font-size: 11px;
    color: var(--td-text-color-secondary);
  }

  &__owner,
  &__due {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  &__owner {
    flex: 1;
  }

  &__due {
    flex-shrink: 0;
    font-variant-numeric: tabular-nums;
  }

  &__due--overdue {
    font-weight: 600;
    color: var(--td-error-color);
  }

  &__spinner {
    position: absolute;
    top: 8px;
    right: 8px;
  }
}
</style>

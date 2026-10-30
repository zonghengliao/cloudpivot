<template>
  <!-- 父任务行专用：子任务完成度（小进度条 + N/M 分数，取代 E6b 的「（3 子·1 完成）」纯文字） -->
  <span class="sub-progress" :title="`子任务 ${done}/${total} 完成`">
    <span class="sub-progress__track">
      <span class="sub-progress__fill" :class="{ 'sub-progress__fill--done': percent >= 100 }" :style="fillStyle" />
    </span>
    <span class="sub-progress__text">{{ text }}</span>
  </span>
</template>

<script setup lang="ts">
/**
 * 子任务进度条（T8-B ③）。
 *
 * 为什么不用 `t-progress`：表格行高只有 40 多 px，`t-progress` 自带内边距与文字层，
 * 塞进 140px 宽的列里会把「N/M」挤到换行；这里用 6px 纯 CSS 小条，
 * 高度和表格行高对齐，`transition` 只在宽度上（不触发布局抖动）。
 *
 * 配色只用主题 token（22 套配色门禁要求 0 裸 hex），满进度换语义成功色。
 */
import { computed } from 'vue';
import { childProgressPercent, childProgressText } from './tree-layout';

const props = withDefaults(defineProps<{ done?: number; total?: number }>(), {
  done: 0,
  total: 0,
});

/** 百分比（取整，越界夹紧） */
const percent = computed(() => childProgressPercent(props.done, props.total));
/** 分数文案 `N/M` */
const text = computed(() => childProgressText(props.done, props.total));
/** 条宽：用行内 style 驱动，避免为每个百分比生成一条 CSS 类 */
const fillStyle = computed(() => ({ width: `${percent.value}%` }));
</script>

<style scoped lang="less">
.sub-progress {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;

  &__track {
    flex: 1;
    min-width: 40px;
    height: 6px;
    overflow: hidden;
    background-color: var(--td-bg-color-component);
    border-radius: var(--td-radius-round);
  }

  &__fill {
    display: block;
    height: 100%;
    background-color: var(--td-brand-color);
    border-radius: var(--td-radius-round);
    transition: width 0.2s linear;

    &--done {
      background-color: var(--td-success-color);
    }
  }

  &__text {
    flex: none;
    font-size: 12px;
    font-variant-numeric: tabular-nums;
    color: var(--td-text-color-secondary);
  }
}
</style>

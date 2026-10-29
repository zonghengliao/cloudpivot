<template>
  <t-tooltip :content="`优先级 ${meta.label}（${meta.name}）`" placement="top">
    <t-tag class="priority-tag" :theme="meta.theme" :variant="variant" :size="size">
      <span class="priority-tag__dot" :style="{ backgroundColor: meta.dotVar }" />
      {{ meta.label }}
    </t-tag>
  </t-tooltip>
</template>

<script setup lang="ts">
/**
 * 优先级标签（P0 最高 → P3 最低）。
 *
 * 配色与中文语义同样集中在 `@/constants/status-map`（`PRIORITY_MAP`），
 * 页面不写死颜色：主题变量驱动，配色方案 / 暗色模式自动跟随。
 */
import { computed } from 'vue';
import { priorityMeta } from '@/constants/status-map';

const props = withDefaults(
  defineProps<{
    /** 后端下发的优先级字面量（`P0`–`P3`，大小写不敏感） */
    priority: string;
    /** t-tag 尺寸 */
    size?: 'small' | 'medium' | 'large';
    /** t-tag 风格 */
    variant?: 'dark' | 'light' | 'outline' | 'light-outline';
  }>(),
  {
    size: 'small',
    variant: 'light',
  },
);

/** 当前优先级的展示元信息 */
const meta = computed(() => priorityMeta(props.priority));
</script>

<style scoped lang="less">
.priority-tag {
  font-variant-numeric: tabular-nums;

  &__dot {
    display: inline-block;
    width: 6px;
    height: 6px;
    margin-right: 4px;
    border-radius: 50%;
    vertical-align: middle;
  }
}
</style>

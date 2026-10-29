<template>
  <t-tag class="status-badge" :theme="meta.theme" :variant="variant" :size="size">
    <span v-if="showDot" class="status-badge__dot" :style="{ backgroundColor: meta.dotVar }" />
    {{ meta.label }}
  </t-tag>
</template>

<script setup lang="ts">
/**
 * 统一状态徽标。
 *
 * 一个组件管三套域（`task` / `announcement` / `import`），文案与配色全部来自
 * `@/constants/status-map`，页面只写：
 *
 * ```vue
 * <StatusBadge domain="task" :status="row.status" />
 * ```
 *
 * 未知状态不白屏：兜底把后端字面量原样显示成灰色徽标（见 `statusMeta`）。
 */
import { computed } from 'vue';
import { statusMeta, type StatusDomain } from '@/constants/status-map';

const props = withDefaults(
  defineProps<{
    /** 徽标域，默认任务域 */
    domain?: StatusDomain;
    /** 后端下发的状态字面量（大小写不敏感） */
    status: string;
    /** t-tag 尺寸 */
    size?: 'small' | 'medium' | 'large';
    /** t-tag 风格 */
    variant?: 'dark' | 'light' | 'outline' | 'light-outline';
    /** 是否显示左侧圆点 */
    showDot?: boolean;
  }>(),
  {
    domain: 'task',
    size: 'small',
    variant: 'light',
    showDot: true,
  },
);

/** 当前状态的展示元信息（文案 + 主题 + 圆点色） */
const meta = computed(() => statusMeta(props.domain, props.status));
</script>

<style scoped lang="less">
.status-badge {
  // 圆点与文字基线对齐：t-tag 内部是 inline-flex，这里只补圆点自身的间距
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

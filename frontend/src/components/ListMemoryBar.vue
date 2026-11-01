<template>
  <div v-if="restored" class="list-memory" :data-testid="testid">
    <t-icon name="history" size="14px" class="list-memory__icon" />
    <span class="list-memory__text" :data-testid="`${testid}-text`">{{ text }}</span>
    <span class="list-memory__dot">·</span>
    <t-link theme="primary" :data-testid="`${testid}-clear`" @click="emit('clear')">清空筛选</t-link>
  </div>
</template>

<script setup lang="ts">
/**
 * 列表记忆提示条（O4 交互要求④）。
 *
 * 恢复筛选是**静默行为**，不提示的话用户会以为「数据怎么少了」。
 * 所以这里固定给一条浅色提示 + 一个「清空筛选」出口：
 * 用户既能感知到「这是上次的条件」，也能一键回到全量。
 *
 * 文案由页面传入（`已恢复上次筛选条件（3 项筛选 · 第 2 页）` 之类），
 * 本组件只负责长得对、有出口。
 */
withDefaults(
  defineProps<{
    /** 是否恢复过（false 时整条不渲染） */
    restored: boolean;
    /** 提示文案 */
    text?: string;
    /** e2e / 单测选择器前缀 */
    testid?: string;
  }>(),
  { text: '已恢复上次筛选条件', testid: 'list-memory' },
);

const emit = defineEmits<{
  /** 点「清空筛选」 */
  (event: 'clear'): void;
}>();

</script>

<style scoped lang="less">
.list-memory {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-top: 8px;
  padding: 4px 10px;
  border-radius: 4px;
  background: var(--td-brand-color-light);
  color: var(--td-brand-color);
  font-size: 12px;
  line-height: 20px;

  &__icon {
    opacity: 0.8;
  }

  &__text {
    color: var(--td-text-color-secondary);
  }

  &__dot {
    color: var(--td-text-color-placeholder);
  }
}
</style>

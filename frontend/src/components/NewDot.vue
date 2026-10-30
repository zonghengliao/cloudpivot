<template>
  <!--
    新内容标记（F1）：两种形态共用一套 token，避免三个列表页各写一份圆点/角标样式。
    · `dot`   —— 6px 主色小圆点（消息中心右上角）
    · `badge` —— `NEW` 文字角标（审批中心「24 小时内到达」）
    颜色只用主题变量：主色面 + 引擎推导的 on-brand 文字色，22 套配色 × 明暗都达标。
  -->
  <span
    class="cp-new"
    :class="`cp-new--${variant}`"
    role="img"
    :aria-label="title"
    :title="title"
  >
    <span v-if="variant === 'dot'" class="cp-new__dot" />
    <template v-else>{{ label }}</template>
  </span>
</template>

<script setup lang="ts">
withDefaults(
  defineProps<{
    /** 形态：小圆点 / NEW 文字角标 */
    variant?: 'dot' | 'badge';
    /** 角标文案（仅 badge） */
    label?: string;
    /** 无障碍名称 / hover 提示 */
    title?: string;
  }>(),
  {
    variant: 'dot',
    label: 'NEW',
    title: '新内容',
  },
);
</script>

<style scoped lang="less">
.cp-new {
  display: inline-flex;
  flex: none;
  align-items: center;
  justify-content: center;

  &--dot {
    width: 6px;
    height: 6px;
  }

  &__dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--td-brand-color);
  }

  &--badge {
    height: 16px;
    padding: 0 4px;
    font-size: 10px;
    font-weight: 700;
    line-height: 16px;
    letter-spacing: 0.02em;
    color: var(--app-on-brand, var(--td-text-color-anti));
    background: var(--td-brand-color);
    border-radius: 3px;
  }
}
</style>

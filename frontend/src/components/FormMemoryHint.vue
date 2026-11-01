<template>
  <div v-if="labels.length > 0" class="form-memory" :data-testid="testid">
    <t-icon name="history" size="14px" class="form-memory__icon" />
    <span class="form-memory__text" :data-testid="`${testid}-text`">已带出上次的{{ labels.join('、') }}</span>
    <span class="form-memory__dot">·</span>
    <t-link theme="default" :data-testid="`${testid}-reset`" @click="emit('reset')">重置</t-link>
  </div>
</template>

<script setup lang="ts">
/**
 * 表单记忆提示条（O1 交互要求③）。
 *
 * 「已带出上次的供应商、仓库 · 重置」—— 记忆是自动填的，
 * 不说一声用户会以为是系统猜的；给「重置」是为了留住「这次想重新选」的人。
 *
 * `labels` 为空（没有记忆 / 编辑态 / 源单已带出）时整条不渲染。
 */
withDefaults(
  defineProps<{
    /** 实际带出的字段中文名（空数组 = 不渲染） */
    labels: string[];
    /** e2e / 单测选择器前缀 */
    testid?: string;
  }>(),
  { testid: 'form-memory' },
);

const emit = defineEmits<{
  /** 点「重置」：清空记忆 */
  (event: 'reset'): void;
}>();

</script>

<style scoped lang="less">
.form-memory {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-top: 12px;
  padding: 4px 10px;
  border-radius: 4px;
  background: var(--td-bg-color-container-hover);
  font-size: 12px;
  line-height: 20px;

  &__icon {
    color: var(--td-text-color-placeholder);
  }

  &__text {
    color: var(--td-text-color-secondary);
  }

  &__dot {
    color: var(--td-text-color-placeholder);
  }
}
</style>

<!--
  三态占位视图（P3-17）：loading / empty / error 统一成一种画法。

  # 为什么要有它

  同一个「列表加载失败」在仓里有七八种写法（`t-alert` + 重试、纯文本、红字、直接白屏），
  空态则是每页自己写 `<t-empty description="暂无数据" />`、插画全靠 t-empty 默认图。
  用户在不同页面看到不同的「空」「错」，会以为是不同的毛病。

  本组件把三态收成一颗：**loading 用骨架屏（可选 spinner）、empty 用 t-empty 内置插画 +
  可选操作按钮、error 用 network-error 插画 + 必带重试**。文案与兜底规则在
  `state-view-pure.ts`（有单测），本文件只负责画。

  # 用法

  ```vue
  <!-- 列表首屏 / 详情区块加载 -->
  <StateView state="loading" :rows="6" />

  <!-- 查无数据，带「新建」主操作 -->
  <StateView
    state="empty"
    :title="filtered ? '没有匹配的记录' : `还没有${title}`"
    description="换个关键词试试，或点「重置」看全部"
    action-text="新建"
    @action="openCreate"
  />

  <!-- 接口失败：description 直接喂后端 msg，点「重试」重拉 -->
  <StateView state="error" :description="errorMessage" testid="sup-error" @action="load" />
  ```

  # 约定（别踩）
  - **error 的按钮默认就是「重试」**；要「不可重试」（403 无权限之类）传 `:action-text="''"`
    —— 显式空串 = 不画按钮，`undefined` = 用默认文案，两者语义不同。
  - **不要把 StateView 塞进 `t-table` 的 `#empty` 之外的地方当 loading 遮罩**：它的 loading 是
    「整块占位」，表格刷新中的转圈仍是 `t-table` 自己的 `loading`。
  - 空态默认渲染 `t-empty`（根类 `.t-empty`），仓里的页面探针（`e2e/_pagecheck.ts`）靠这个类
    判「空态」，自己另写一套 div 会让探针抓不到。
-->
<template>
  <div
    class="sv"
    :class="[`sv--${resolved.kind}`, { 'sv--compact': compact }]"
    :data-testid="testid"
    :data-state="resolved.kind"
  >
    <!-- ① loading：骨架屏（默认）/ spinner。
         骨架屏的行列必须传**对象**：`t-skeleton` 的 `rowCol` 项会做 `'width' in item`，
         传 `'100%'` 这种字符串会在渲染期抛 TypeError，整棵子树挂不上（判据在 state-view-pure.ts）。 -->
    <div v-if="resolved.kind === 'loading'" class="sv__loading">
      <div v-if="resolved.variant === 'skeleton'" class="sv__skeleton" role="status" :aria-label="resolved.title">
        <t-skeleton theme="paragraph" :row-col="resolved.rowCol" animation="gradient" />
        <p v-if="description" class="sv__loading-text">{{ resolved.description }}</p>
      </div>
      <div v-else class="sv__spinner" role="status">
        <t-loading :loading="true" size="small" />
        <span class="sv__spinner-text">{{ resolved.title }}</span>
      </div>
    </div>

    <!-- ② empty / ③ error：t-empty 内置插画 + 标题 + 描述 + 操作 -->
    <t-empty
      v-else
      class="sv__empty"
      :type="resolved.emptyType"
      :title="resolved.title"
      :description="resolved.description || undefined"
    >
      <template v-if="resolved.showAction || resolved.secondaryActionText || $slots.actions" #action>
        <div class="sv__actions">
          <slot name="actions">
            <t-button
              v-if="resolved.showAction"
              :theme="actionTheme"
              size="small"
              :loading="actionLoading"
              :data-testid="testid ? `${testid}-action` : undefined"
              @click="emit('action')"
            >
              {{ resolved.actionText }}
            </t-button>
            <t-button
              v-if="resolved.secondaryActionText"
              variant="outline"
              size="small"
              :data-testid="testid ? `${testid}-secondary` : undefined"
              @click="emit('secondary')"
            >
              {{ resolved.secondaryActionText }}
            </t-button>
          </slot>
        </div>
      </template>
    </t-empty>
  </div>
</template>

<script setup lang="ts">
/**
 * 三态占位视图（P3-17 试点：ListPage / 甘特图 / 流程联动规则）。
 *
 * 判据与默认文案在 `state-view-pure.ts`，本文件只做渲染与事件。
 */
import { computed } from 'vue';
import { resolveStateView, type StateViewState } from './state-view-pure';

const props = defineProps<{
  /** 当前状态：loading / empty / error（非法值按 empty 画，见 pure 规则 1） */
  state: StateViewState;
  /** loading 形态：skeleton（默认，整块占位）/ spinner（小范围、抽屉里用） */
  variant?: 'skeleton' | 'spinner';
  /** 骨架屏行数（1~12，默认 4） */
  rows?: number;
  /** loading 的补充说明（骨架屏下方一行小字） */
  description?: string;
  /** empty / error 标题 */
  title?: string;
  /** 主操作文案：empty 不给不画；error 不给默认「重试」；显式 `''` 不画按钮 */
  actionText?: string;
  /** 次操作文案（outline 按钮） */
  secondaryActionText?: string;
  /** 主操作按钮转圈（防连点，如「重试」请求中） */
  actionLoading?: boolean;
  /** 主操作按钮主题，默认 primary */
  actionTheme?: 'primary' | 'default' | 'success' | 'warning' | 'danger';
  /** 插画类型（t-empty 的 type）：empty / success / fail / network-error / maintenance */
  emptyType?: 'empty' | 'success' | 'fail' | 'network-error' | 'maintenance';
  /** 紧凑内边距（卡片 / 抽屉里一小格） */
  compact?: boolean;
  /** 根节点 data-testid（`-action` / `-secondary` 会跟后缀） */
  testid?: string;
}>();

const emit = defineEmits<{
  /** 主操作（重试 / 新建 / 重置…） */
  action: [];
  /** 次操作 */
  secondary: [];
}>();

const resolved = computed(() =>
  resolveStateView({
    state: props.state,
    variant: props.variant,
    rows: props.rows,
    title: props.title,
    description: props.description,
    actionText: props.actionText,
    secondaryActionText: props.secondaryActionText,
    emptyType: props.emptyType,
  }),
);
</script>

<style scoped lang="less">
.sv {
  display: flex;
  flex-direction: column;
  justify-content: center;
  width: 100%;
  padding: 48px 16px;

  &--compact {
    padding: 24px 12px;
  }
}

.sv__loading {
  width: 100%;
}

.sv__skeleton {
  width: 100%;
  max-width: 880px;
  margin: 0 auto;
}

.sv__loading-text {
  margin: 12px 0 0;
  font-size: 13px;
  color: var(--td-text-color-secondary);
  text-align: center;
}

.sv__spinner {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  color: var(--td-text-color-secondary);
}

.sv__spinner-text {
  font-size: 13px;
}

.sv__empty {
  padding: 0;
}

.sv__actions {
  display: inline-flex;
  align-items: center;
  gap: 8px;
}
</style>

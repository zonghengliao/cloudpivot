<!--
  DocActionBar.vue —— 详情页「关键动作固定位」（ARCH-3 §6 R-P1-5 · 用户原话「不能满屏去找按钮」）

  # 为什么要有它

  改造前四个单据详情页（采购单 / 收货单 / 应付单 / 调拨单）的关键动作散落各处：
  有的在抽屉头部、有的在列表行上、有的要滚到底才看得见。操作员找一个「作废」要点开
  三四处。这个组件把「本页最重要的 1–3 个动作」固定在抽屉顶部的**吸顶条**里：
  `position: sticky` ⇒ 往下翻明细 / 审批时间线 / 附件时它一直在。

  # 用法

  ```vue
  <DocActionBar title="关键操作" testid="purchase-action-bar">
    <t-button size="small" ...>提交审批</t-button>
    <t-button size="small" theme="danger" ...>作废</t-button>
  </DocActionBar>
  ```

  标题在左、按钮在右（`margin-right: auto` 推开），窄屏自动换行不遮挡内容。
-->
<template>
  <div class="doc-action-bar" :data-testid="testid">
    <span class="doc-action-bar__label">
      <t-icon name="control-platform" size="16px" /> {{ title }}
    </span>
    <div class="doc-action-bar__actions">
      <slot />
    </div>
  </div>
</template>

<script setup lang="ts">
withDefaults(
  defineProps<{
    /** 左侧标题（一般写「关键操作」或页面名） */
    title?: string;
    /** 自动化测试的 testid */
    testid?: string;
  }>(),
  { title: '关键操作', testid: 'doc-action-bar' },
);
</script>

<style scoped>
/* 吸顶条：跟随抽屉内容区滚动，永远可见（用户的原始诉求） */
.doc-action-bar {
  position: sticky;
  top: -12px;
  z-index: 30;
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin: -4px 0 12px;
  padding: 8px 12px;
  background: var(--td-bg-color-container, #fff);
  border-bottom: 1px solid var(--td-component-stroke, #e7e7e7);
  box-shadow: 0 2px 8px rgb(0 0 0 / 6%);
}

.doc-action-bar__label {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  margin-right: auto; /* 把按钮推到右边 */
  font-weight: 600;
  color: var(--td-text-color-primary, #000000e6);
}

.doc-action-bar__actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}
</style>

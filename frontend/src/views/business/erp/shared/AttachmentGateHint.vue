<!--
  附件必传缺口的**按钮旁红字**（ATT-08）。

  # 为什么单独一个组件

  同一行红字要在多个单据页出现（发票开票 / 报销提交 / 收货提交 …），而且它
  有个必须统一的细节：**文案完全来自后端**（`GET /attachment/check` 的
  `missing[].hint`），前端不拼「缺少 X」这类句子 —— 否则前端说「缺发票影像」、
  后端 400 说「发票必须上传发票影像（pdf/ofd/xml）」，用户会以为是两件事。

  组件只负责「有就画、没有就不占位」，判定在 `attachment-gate-pure.ts`。

  # 用法

  ```vue
  <AttachmentGateHint :hint="attachGate.hint" testid="inv-detail-gate-hint" />
  ```
-->
<template>
  <span
    v-if="hint"
    class="attach-gate-hint"
    :data-testid="testid"
    :title="hint"
  >
    {{ hint }}
  </span>
</template>

<script setup lang="ts">
withDefaults(
  defineProps<{
    /** 后端给的红字；`null` / 空串 = 不渲染（不留空占位） */
    hint?: string | null;
    /** `data-testid`（各页面不同，便于 e2e 定位） */
    testid?: string;
  }>(),
  { hint: null, testid: 'attach-gate-hint' },
);
</script>

<style scoped>
/* 用 TDesign 的 error 色变量：换主题时跟着走，不写死 #d54941 */
.attach-gate-hint {
  color: var(--td-error-color, #d54941);
  font-size: 12px;
  line-height: 20px;
}
</style>

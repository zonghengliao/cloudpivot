<!--
  现金流量表页（菜单 `/business/erp/report/cash-flow`，`sys_menu` …001018）。

  这一页有三个 **tab**，按权限显示（`V0415` / `V0417` 只种了按钮码，没有新开菜单页 ——
  侧边栏仍然只有「现金流量表」一项）：

  | tab | 内容 | 需要的权限码 |
  |-----|------|--------------|
  | 现金流底稿 | 逐行编制底稿 + 勾稽（M1-18 / `V0414` `V0415`） | `business:erp:cashflow-worksheet:list` |
  | 报表预览   | 由底稿汇总出的三栏现金流量表 + 导出（M1-16 / `V0417`） | `business:erp:cashflow-statement:list` |
  | 按凭证编制 | 老的「按凭证自动分类」简化版报表（ERP-R3） | 页面码 `business:erp:report:cash-flow` |

  # 为什么三件事挤一页而不是三个菜单
  它们回答的是同一个问题（本期现金流量是多少），差别只在**编制路径**：
  底稿是「人来定」（会计逐行编）、按凭证是「算法分」（现金类科目 + 对方科目规则）、
  报表预览是「底稿的产出物」。三张菜单会让侧边栏出现三个近义项，且底稿 / 报表本来
  就是同一次编制的前后两步 —— `V0415` / `V0417` 的迁移注释也是这么定的。

  # 权限自适应
  只授了底稿码的人看不到「报表预览」（GET 会 403），只授了报表码的人看不到底稿 ——
  按 `userStore.hasPermission` 过滤 tab，而不是让用户点进去吃 403。
-->
<template>
  <div class="cf-tabs-page">
    <t-tabs v-model="active" :destroy-on-hide="false" data-testid="cf-tabs">
      <t-tab-panel v-if="canWorksheet" value="worksheet" label="现金流底稿" data-testid="cf-tab-worksheet">
        <CashflowWorksheetPanel @preview="handlePreview" />
      </t-tab-panel>
      <t-tab-panel v-if="canStatement" value="statement" label="报表预览" data-testid="cf-tab-statement">
        <CashflowStatementPanel :period="previewPeriod" @goto-worksheet="handleGotoWorksheet" />
      </t-tab-panel>
      <t-tab-panel value="voucher" label="按凭证编制" data-testid="cf-tab-voucher">
        <VoucherCashflowPanel />
      </t-tab-panel>
    </t-tabs>
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';

import { useUserStore } from '@/stores/user';
import {
  PERM_STATEMENT_LIST,
  PERM_WORKSHEET_LIST,
} from '@/views/business/erp/report/cash-flow/worksheet-logic';
import CashflowStatementPanel from '@/views/business/erp/report/cash-flow/statement-panel.vue';
import CashflowWorksheetPanel from '@/views/business/erp/report/cash-flow/worksheet-panel.vue';
import VoucherCashflowPanel from '@/views/business/erp/report/cash-flow/voucher-panel.vue';

const userStore = useUserStore();

const canWorksheet = computed(() => userStore.hasPermission(PERM_WORKSHEET_LIST));
const canStatement = computed(() => userStore.hasPermission(PERM_STATEMENT_LIST));

/** 默认落第一个**有权限**的 tab：有底稿码就先进底稿（编制是主线），否则进按凭证编制 */
const active = ref(canWorksheet.value ? 'worksheet' : 'voucher');
/** 「报表预览」要看的期间（从底稿页点「报表预览」带过来；空 = 用报表页自己的下拉） */
const previewPeriod = ref('');

function handlePreview(period: string) {
  previewPeriod.value = period;
  if (!canStatement.value) {
    MessagePlugin.warning('当前账号没有「出表查看」权限（business:erp:cashflow-statement:list），看不到报表预览');
    return;
  }
  active.value = 'statement';
}

function handleGotoWorksheet(period: string) {
  if (!canWorksheet.value) {
    MessagePlugin.warning('当前账号没有「底稿查看」权限（business:erp:cashflow-worksheet:list），打不开底稿');
    return;
  }
  previewPeriod.value = period;
  active.value = 'worksheet';
}
</script>

<style scoped>
.cf-tabs-page {
  padding: 0;
}
</style>

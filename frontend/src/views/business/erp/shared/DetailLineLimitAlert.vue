<!--
  明细超行数上限的黄字提示（P1-04）。

  # 为什么单独一个组件

  同一句话要在 7 个单据页的明细表上方出现（入库 / 采购 / 调拨 / 出库 / 询价 /
  采购申请 / 采购退货）。各页自己拼一遍句子，早晚会有一页和后端 400 的文案分叉。

  # 为什么只提示、不阻断

  卡面决策是「明确拒绝 + 可操作提示」：前端**不拦死**编辑（明细可能是从来源单
  带出来的，删到 200 行以内本身合法），只在超限时把「超了多少、下一步做什么」
  说清楚；保存时后端按 `MAX_LINES` 400 兜底。

  # 为什么没有跳转按钮

  初版这里挂过一个指向导入向导的跳转按钮。导入向导里根本没有这 7 个单据
  （`register_import` 9 个定义无一单据），点进去只会二次挫折；换成「去列表页新建」
  又会丢掉用户此刻的 301 行明细。所以只给句子，不给按钮
  —— 理由见 `docs/单据明细导入口径-决策.md` §3.4。

  # 文案来源

  `shared/detail-line-limit.ts`（与后端 `business/shared/doc_lines.rs` 同口径）。

  # 用法

  ```vue
  <DetailLineLimitAlert :count="form.items.length" :limit="MAX_LINES" testid="stockout-line-limit" />
  ```
-->
<template>
  <t-alert
    v-if="over"
    theme="warning"
    class="detail-line-limit"
    :data-testid="testid"
  >
    <template #message>
      <span>{{ message }}</span>
    </template>
  </t-alert>
</template>

<script setup lang="ts">
import { computed } from 'vue';

import {
  DETAIL_MAX_LINES,
  detailLineLimitMessage,
  overDetailLineLimit,
} from './detail-line-limit';

const props = withDefaults(
  defineProps<{
    /** 当前明细行数（各页传 `form.items.length` / `createForm.lines.length`） */
    count: number;
    /** 该单据的后端上限；缺省 200（与后端 `MAX_LINES` 同值） */
    limit?: number;
    /** `data-testid`（各页不同，便于 e2e 定位） */
    testid?: string;
  }>(),
  { limit: DETAIL_MAX_LINES, testid: 'detail-line-limit' },
);

const over = computed(() => overDetailLineLimit(props.count, props.limit));
const message = computed(() => detailLineLimitMessage(props.count, props.limit));
</script>

<style scoped>
.detail-line-limit {
  margin: 8px 0;
}
</style>

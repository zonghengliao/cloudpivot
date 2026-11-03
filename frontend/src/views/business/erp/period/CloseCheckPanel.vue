<template>
  <!--
    结账弹窗的检查清单面板（PERIOD-CLOSE）。

    ★ 表格 / 每行[去处理][催办] / 全部催办 / 提醒项的**实现**都在
    `../shared/BlockerCheckPanel.vue`（YEAR-END 卡把它抽成共用组件，年结页也在用）。
    本文件只保留「期间」这一侧的三件事：
      ① `props.check` → `blockers` / `warnings` 的映射与顶部摘要文案；
      ② 催办地址绑到 `POST /business/erp/period/{id}/remind`；
      ③ `data-testid` 前缀固定 `period`（真机脚本与历史证据都按这些 id 抓页面）。

    ★ 列表仍然**不能**挂在 `t-alert` 的 `#description` 槽上（TDesign 1.19 会静默丢弃），
    这条约束在共用组件里落地并写明了原因。
  -->
  <BlockerCheckPanel
    ref="panel"
    :blockers="blockers"
    :warnings="warnings"
    :remind="remindFn"
    :summary="summary"
    :summary-theme="summaryTheme"
    testid-prefix="period"
    show-messages
    @goto="$emit('goto', $event)"
    @reminded="$emit('reminded', $event)"
  />
</template>

<script setup lang="ts">
/**
 * 结账弹窗的**检查清单面板**（PERIOD-CLOSE）。
 *
 * 用户原话 → 落地方式见 `shared/BlockerCheckPanel.vue` 头注释（列单据 / 标处理人 /
 * 一键催办）。本组件是它面向「会计期间」的薄封装。
 */
import { computed, ref } from 'vue';
import { remindPeriod, type PeriodCloseCheck, type PeriodRemindResult } from '@/api/business/erp';
import BlockerCheckPanel from '../shared/BlockerCheckPanel.vue';
import { closeCheckSummary, type BlockerRemindFn, type BlockingBlocker } from './period-logic';

const props = withDefaults(
  defineProps<{
    /** 预检结果（`null` = 还在加载，不渲染面板） */
    check: PeriodCloseCheck | null;
    /** 期间 id（催办接口用） */
    periodId: string;
    /** 有催办权限时才显示催办按钮 */
    canRemind?: boolean;
  }>(),
  { canRemind: true },
);

const emit = defineEmits<{
  /** 点「去处理」→ 父组件决定用 router 还是新窗口 */
  (event: 'goto', route: string): void;
  /** 催办完成 → 父组件 toast */
  (event: 'reminded', result: PeriodRemindResult): void;
}>();

const panel = ref<InstanceType<typeof BlockerCheckPanel> | null>(null);

const blockers = computed<BlockingBlocker[]>(() => props.check?.blockers ?? []);
const warnings = computed<BlockingBlocker[]>(() => props.check?.warnings ?? []);
const summary = computed(() =>
  closeCheckSummary(props.check ?? { can_close: false, blockers: [] }),
);
const summaryTheme = computed<'success' | 'warning'>(() =>
  props.check?.can_close ? 'success' : 'warning',
);

/**
 * 催办：**只传「哪一类 + 哪几张」**，收件人由后端按明细的 owner 现算
 * （前端改不动收件人；页面停留期间单据被人处理了也不会催错人）。
 *
 * 没权限时传 `undefined` —— 共用面板据此**不渲染**催办按钮与「全部催办」。
 */
const remindFn = computed<BlockerRemindFn | undefined>(() => {
  if (!props.canRemind) {
    return undefined;
  }
  const periodId = props.periodId;
  return (payload) => remindPeriod(periodId, payload);
});

/** 供父组件在重新预检后清空「已催」标记 */
defineExpose({
  reset(): void {
    panel.value?.reset();
  },
});

// `emit` 只在类型声明里用；模板里走 `$emit`。保留引用避免 `noUnusedLocals` 报错。
void emit;
</script>

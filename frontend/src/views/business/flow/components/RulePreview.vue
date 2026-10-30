<template>
  <div class="rp" :class="{ 'rp--error': !report.ok }">
    <!-- 头部：标题 + 结论 + 错误码 -->
    <div class="rp__head">
      <span class="rp__title">规则预览</span>
      <span v-if="report.ok" class="rp__badge rp__badge--ok">校验通过</span>
      <span v-else class="rp__badge rp__badge--error">校验未通过 {{ report.issues.length }} 条</span>
      <span class="rp__spacer" />
      <span v-if="report.codes.length > 0" class="rp__codes">错误码 {{ report.codes.join(' / ') }}</span>
    </div>

    <!-- 人话翻译 -->
    <div class="rp__body" :class="{ 'rp__body--empty': preview.empty }">
      <p
        v-for="(line, index) in preview.lines"
        :key="`${index}-${line}`"
        class="rp__line"
        :class="{ 'rp__line--then': index > 0 }"
      >
        {{ line }}
      </p>
    </div>

    <!-- 结构计数：n/limit -->
    <ul class="rp__meters">
      <li class="rp__meter">
        结构
        <span :class="{ 'rp__meter-value--over': over.depth }">{{ meters.depth }}</span>
      </li>
      <li class="rp__meter">
        节点
        <span :class="{ 'rp__meter-value--over': over.nodes }">{{ meters.nodes }}</span>
      </li>
      <li class="rp__meter">
        规则
        <span :class="{ 'rp__meter-value--over': over.rules }">{{ meters.rules }}</span>
      </li>
      <li v-if="report.counts.targets > 0 || hasTargets" class="rp__meter">
        审批人
        <span :class="{ 'rp__meter-value--over': over.targets }">{{ meters.targets }}</span>
      </li>
    </ul>

    <!-- ⚠ 提示（空字段 / 目标为空 / 超限） -->
    <ul v-if="preview.warnings.length > 0" class="rp__warns">
      <li v-for="warning in preview.warnings.slice(0, 5)" :key="warning" class="rp__warn">
        {{ warning }}
      </li>
      <li v-if="preview.warnings.length > 5" class="rp__warn">
        … 还有 {{ preview.warnings.length - 5 }} 条
      </li>
    </ul>

    <!-- 逐条问题（带后端码）：点一行把面板滚到那条，人工核对文案用 -->
    <ul v-if="report.issues.length > 0" class="rp__issues">
      <li v-for="(issue, index) in report.issues.slice(0, 4)" :key="`${index}-${issue.message}`" class="rp__issue">
        <t-tag v-if="issue.code !== null" size="small" theme="danger" variant="light">
          {{ issue.code }}
        </t-tag>
        <span class="rp__issue-text">{{ issue.reason }}</span>
      </li>
      <li v-if="report.issues.length > 4" class="rp__issue">
        <span class="rp__issue-text">… 还有 {{ report.issues.length - 4 }} 条</span>
      </li>
    </ul>
  </div>
</template>

<script setup lang="ts">
/**
 * 规则预览面板（批次 D · P1-B3 · ② 人话翻译 + ① 计数）。
 *
 * # 位置
 *
 * 挂在属性面板的「条件表达式」下面（`NodeProps.vue`，连线分支），
 * 左边是 `ConditionTree`（P1-B1）、下面是 `ThenTargetEditor`（P1-B2）——
 * 三者共用同一份 `edge.condition` / `then` 数据，这里**只读不写**，不产生任何 emit。
 *
 * # 为什么计数也放这里
 *
 * 卡面要求预览区同时显示 `⚠ 结构：8/8 层`、`节点：23/64`。
 * 计数与「超没超」都来自 `rule-validation.ts::validateRule`，
 * 和红框判定是同一份结果 —— 不会出现「计数红了但框没红」这种自相矛盾。
 */
import { computed } from 'vue';
import type { FlowEdge, FlowNode } from '@/api/business/flow-api';
import { translateRule, type RulePreviewModel } from './rule-preview';
import type { ThenTarget } from './then-target';
import {
  countText,
  isOverLimit,
  validateRule,
  type RuleCounts,
  type RuleLimits,
  type RuleReport,
} from './rule-validation';

/** 组件入参 */
const props = withDefaults(
  defineProps<{
    /** 后端 `edge.condition` 原文 */
    condition?: unknown;
    /** then 目标（`undefined` = 不展示 THEN 段） */
    targets?: ThenTarget[] | undefined;
    /** 连线（给了才有「边 e1（n1 → n2）…」的完整后端文案） */
    edge?: FlowEdge | null;
    /** 目标节点（给了才有「节点 n3（…）…」的完整后端文案） */
    node?: FlowNode | null;
    /** 字段中文名表（`form.amount → 金额`） */
    fieldLabels?: Record<string, string>;
    /** 外部算好的体检结果（免得重复算） */
    report?: RuleReport | null;
  }>(),
  { edge: null, node: null, fieldLabels: () => ({}), report: null },
);

/** 规则 id（后端 expression 的 id，界面只预览第 1 条，固定 r_1） */
const RULE_ID = 'r_1';

/** 体检结果（外部给了就用外部那份，保证与红框同源） */
const report = computed<RuleReport>(() => {
  if (props.report) return props.report;
  return validateRule({
    condition: props.condition,
    targets: props.targets,
    edge: props.edge,
    node: props.node,
    ruleId: RULE_ID,
  });
});

/** 人话预览 */
const preview = computed<RulePreviewModel>(() =>
  translateRule({
    condition: props.condition,
    targets: props.targets,
    fieldLabels: props.fieldLabels,
    report: report.value,
    ruleId: RULE_ID,
  }),
);

/** 计数文本（`8/8 层` / `23/64` / `3/32 条`） */
const meters = computed<{ depth: string; nodes: string; rules: string; targets: string }>(() => {
  const base = countText(report.value);
  const limits: RuleLimits = report.value.limits;
  return { ...base, targets: `${report.value.counts.targets}/${limits.targets}` };
});

/** 超限标记（决定红不红） */
const over = computed<{ depth: boolean; nodes: boolean; rules: boolean; targets: boolean }>(() => ({
  ...isOverLimit(report.value),
  targets: report.value.counts.targets > report.value.limits.targets,
}));

/** 有 then 段（没给 targets 时连「审批人」计数都不显示） */
const hasTargets = computed<boolean>(() => props.targets !== undefined);

/** 计数（给外部调试用） */
const counts = computed<RuleCounts>(() => report.value.counts);
defineExpose({ report, preview, counts });
</script>

<style scoped lang="less">
.rp {
  padding: 8px 10px;
  margin-top: 8px;
  border: 1px solid var(--td-component-stroke);
  border-radius: var(--td-radius-default);
  background: var(--td-bg-color-secondarycontainer);

  &--error {
    border-color: var(--td-error-color);
  }

  &__head {
    display: flex;
    gap: 8px;
    align-items: center;
    padding-bottom: 6px;
    border-bottom: 1px dashed var(--td-component-stroke);
  }

  &__title {
    font-size: 13px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__badge {
    padding: 0 6px;
    font-size: 11px;
    line-height: 18px;
    border-radius: var(--td-radius-round);
  }

  &__badge--ok {
    color: var(--td-success-color);
    background: var(--td-success-color-1);
  }

  &__badge--error {
    color: var(--td-error-color);
    background: var(--td-error-color-1);
  }

  &__spacer {
    flex: 1;
  }

  &__codes {
    font-family: var(--td-font-family-mono, monospace);
    font-size: 11px;
    color: var(--td-text-color-placeholder);
  }

  &__body {
    padding: 8px 0 2px;
  }

  &__body--empty {
    color: var(--td-text-color-placeholder);
  }

  &__line {
    margin: 0;
    font-size: 12px;
    line-height: 1.75;
    color: var(--td-text-color-primary);
  }

  &__line--then {
    font-weight: 600;
    color: var(--td-brand-color);
  }

  &__meters {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
    padding: 4px 0 0;
    margin: 0;
    list-style: none;
  }

  &__meter {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__meter-value--over {
    font-weight: 600;
    color: var(--td-error-color);
  }

  &__warns {
    padding: 6px 8px 6px 20px;
    margin: 6px 0 0;
    list-style: disc;
    background: var(--td-warning-color-1);
    border-radius: var(--td-radius-default);
  }

  &__warn {
    font-size: 12px;
    line-height: 1.6;
    color: var(--td-warning-color);
  }

  &__issues {
    padding: 6px 0 0;
    margin: 6px 0 0;
    list-style: none;
    border-top: 1px dashed var(--td-component-stroke);
  }

  &__issue {
    display: flex;
    gap: 6px;
    align-items: flex-start;
    padding: 2px 0;
  }

  &__issue-text {
    font-size: 12px;
    line-height: 1.6;
    color: var(--td-text-color-secondary);
    word-break: break-all;
  }
}
</style>

<!--
  CancelImpactDialog.vue —— 联动作废的「影响面确认」弹窗（ARCH-3 §6 R-P1-5）

  # 用户看到什么

  ```
  作废 PO-20261031-000011 将影响以下 4 张单据（本次会执行 4 个动作）

  ⚠ 1 张已过账收货单 IN-20261102-000001（¥16.95）  → 作废（回滚三本账 + 退序列号）
  ⚠ 1 张进项凭证   PZ-20261102-000001（¥16.95）    → 自动红冲（生成红字凭证）
  ⚠ 1 张应付单     AP-20261102-000001（¥16.95）    → 自动作废
  ⚠ 1 张采购订单   PO-20261031-000011（¥16.95）    → 作废采购单

  作废原因：[____________________]（必填，≥4 字）
                                     [取消] [一键按顺序处理]
  ```

  # 铁律

  * **必须先看到影响面再动手**：弹窗打开就去 `GET …/cancel-impact`（只读），
    后端刻意不做「点作废就顺手改下游」的静默级联；
  * `can_cancel = false`（有拦路虎）时「一键处理」置灰，把 `blockers` 逐条显示出来；
  * 原因必填（≥4 字）—— 会计单据作废必须能追溯；
  * 成功后发 `done` 事件让父页面刷新，并把后端返回的 message 原样 toast。
-->
<template>
  <t-dialog
    :visible="visible"
    :header="headerText"
    width="640px"
    placement="center"
    :close-on-overlay-click="false"
    :confirm-btn="null"
    :cancel-btn="null"
    data-testid="cancel-impact-dialog"
    @close="close"
  >
    <div v-if="loading" class="cip__loading">
      <t-loading size="small" /> 正在分析影响面…
    </div>

    <t-alert v-else-if="error" theme="error" :message="error" data-testid="cancel-impact-error" />

    <template v-else-if="impact">
      <p class="cip__summary" data-testid="cancel-impact-summary">{{ impact.summary }}</p>

      <t-alert
        v-if="impact.blockers.length > 0"
        theme="error"
        class="cip__blockers"
        data-testid="cancel-impact-blockers"
      >
        <template #message>
          <div v-for="(blocker, index) in impact.blockers" :key="index">{{ blocker }}</div>
        </template>
      </t-alert>

      <div class="cip__steps" data-testid="cancel-impact-steps">
        <div
          v-for="(step, index) in impact.steps"
          :key="`${step.doc_type}-${step.doc_id}-${index}`"
          class="cip__step"
          :class="`cip__step--${step.action.toLowerCase()}`"
          :data-testid="`cancel-impact-step-${index}`"
        >
          <t-icon :name="iconOf(step.action)" :class="`cip__icon cip__icon--${step.action.toLowerCase()}`" />
          <div class="cip__body">
            <div class="cip__line">
              <span class="cip__type">{{ step.doc_type_label }}</span>
              <span class="cip__no mono">{{ step.doc_no }}</span>
              <span class="cip__amount">{{ money(step.amount) }}</span>
              <t-tag size="small" :theme="tagThemeOf(step.action)" variant="light">{{ step.action_label }}</t-tag>
            </div>
            <div class="cip__detail muted">{{ step.detail }}</div>
          </div>
        </div>
      </div>

      <div class="cip__reason">
        <div class="cip__reason-label">
          作废原因 <span class="cip__required">*</span>
          <span class="muted">（必填，≥{{ MIN_REASON }} 字；会写进每张单据的状态日志）</span>
        </div>
        <t-textarea
          v-model="reason"
          :maxlength="MAX_REASON"
          :autosize="{ minRows: 2, maxRows: 4 }"
          placeholder="例如：客户取消订单，供应商同意全部退货"
          data-testid="cancel-impact-reason"
        />
      </div>

      <p class="cip__note muted" data-testid="cancel-impact-note">{{ impact.note }}</p>
    </template>

    <template #footer>
      <div class="cip__footer">
        <t-button variant="outline" :disabled="submitting" data-testid="cancel-impact-dismiss" @click="close">
          取消
        </t-button>
        <t-button
          theme="danger"
          :loading="submitting"
          :disabled="!canSubmit"
          data-testid="cancel-impact-confirm"
          @click="confirm"
        >
          一键按顺序处理
        </t-button>
      </div>
    </template>
  </t-dialog>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import {
  fetchCancelImpact,
  postCancelCascade,
  type CancelCascadeRes,
  type CancelDocType,
  type CancelImpactRes,
} from '@/api/business/erp/cancel-cascade';

/** 原因最小长度：与后端 `cancel_cascade::MIN_REASON_LEN` 一致 */
const MIN_REASON = 4;
/** 原因上限：与后端 `stockin_dto::MAX_REASON_LEN` 一致 */
const MAX_REASON = 500;

const props = defineProps<{
  /** 弹窗可见（v-model:visible） */
  visible: boolean;
  /** 单据类型（`purchase-order` / `stock-in` / `payable`） */
  docType: CancelDocType;
  /** 单据 id */
  docId: string;
  /** 单号（只在「加载中」的标题里用，影响面里的单号来自后端） */
  docNo?: string;
}>();

const emit = defineEmits<{
  (event: 'update:visible', value: boolean): void;
  (event: 'done', result: CancelCascadeRes): void;
}>();

const loading = ref(false);
const submitting = ref(false);
const error = ref('');
const impact = ref<CancelImpactRes | null>(null);
const reason = ref('');

const headerText = computed(() =>
  impact.value
    ? `作废影响分析 · ${impact.value.doc_no}`
    : `作废影响分析${props.docNo ? ` · ${props.docNo}` : ''}`,
);

const canSubmit = computed(
  () =>
    !!impact.value &&
    impact.value.can_cancel &&
    reason.value.trim().length >= MIN_REASON &&
    !submitting.value,
);

/** 金额展示：后端给定点字符串（`NUMERIC`），保留 2 位 + 千分位。 */
function money(raw: string | number | null | undefined): string {
  if (raw === null || raw === undefined || raw === '') return '¥0.00';
  const value = Number(raw);
  if (!Number.isFinite(value)) return `¥${raw}`;
  return `¥${value.toLocaleString('zh-CN', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
}

function iconOf(action: string): string {
  if (action === 'BLOCKED') return 'error-circle';
  if (action === 'SKIP') return 'check-circle';
  return 'arrow-right';
}

function tagThemeOf(action: string): 'danger' | 'success' | 'warning' {
  if (action === 'BLOCKED') return 'danger';
  if (action === 'SKIP') return 'success';
  return 'warning';
}

function close(): void {
  emit('update:visible', false);
}

async function load(): Promise<void> {
  loading.value = true;
  error.value = '';
  impact.value = null;
  reason.value = '';
  try {
    impact.value = await fetchCancelImpact(props.docType, props.docId);
  } catch (err) {
    error.value =
      err instanceof ApiError ? err.message : `影响面加载失败：${(err as Error).message}`;
  } finally {
    loading.value = false;
  }
}

async function confirm(): Promise<void> {
  if (!canSubmit.value || !impact.value) return;
  submitting.value = true;
  try {
    const result = await postCancelCascade(props.docType, props.docId, reason.value.trim());
    await MessagePlugin.success(result.message);
    emit('done', result);
    close();
  } catch (err) {
    // 拦路虎 / 中途失败都在这里：后端返回的报文已经是「第 N 步「xxx」失败：…整笔回滚」
    await MessagePlugin.error(
      err instanceof ApiError ? err.message : `联动作废失败：${(err as Error).message}`,
    );
    // 失败后重拉影响面：拦路虎可能刚被人处理掉，也可能刚被人抢先作废
    await load();
  } finally {
    submitting.value = false;
  }
}

watch(
  () => [props.visible, props.docId] as const,
  ([visible]) => {
    if (visible) void load();
  },
  { immediate: true },
);
</script>

<style scoped>
.cip__loading {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 24px 0;
  justify-content: center;
}

.cip__summary {
  margin: 0 0 12px;
  font-weight: 600;
}

.cip__blockers {
  margin-bottom: 12px;
}

.cip__steps {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-bottom: 12px;
  max-height: 340px;
  overflow: auto;
}

.cip__step {
  display: flex;
  gap: 8px;
  padding: 8px 10px;
  border-radius: 6px;
  border: 1px solid var(--td-component-stroke, #e7e7e7);
  background: var(--td-bg-color-container, #fff);
}

.cip__step--blocked {
  border-color: var(--td-error-color-3, #ffb5b5);
  background: var(--td-error-color-1, #fff0f0);
}

.cip__step--skip {
  opacity: 0.72;
}

.cip__icon {
  margin-top: 2px;
}

.cip__icon--execute {
  color: var(--td-warning-color, #ed7b2f);
}

.cip__icon--blocked {
  color: var(--td-error-color, #d54941);
}

.cip__icon--skip {
  color: var(--td-success-color, #2ba471);
}

.cip__body {
  flex: 1;
  min-width: 0;
}

.cip__line {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}

.cip__type {
  font-weight: 600;
}

.cip__no {
  font-family: var(--td-font-family-mono, monospace);
}

.cip__amount {
  color: var(--td-text-color-secondary, #00000099);
}

.cip__detail {
  margin-top: 2px;
  font-size: 12px;
}

.cip__reason {
  margin-top: 4px;
}

.cip__reason-label {
  margin-bottom: 4px;
  font-weight: 600;
}

.cip__required {
  color: var(--td-error-color, #d54941);
}

.cip__note {
  margin: 8px 0 0;
  font-size: 12px;
}

.cip__footer {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
</style>

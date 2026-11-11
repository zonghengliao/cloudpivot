<!--
  高危动作确认框（COST-B3-11 / ARCH-9 §6.2.2 ⑤ + §4.8 反向路径）。

  设计 §6.3 点名的第三个页内组件是「结算确认框」；红冲（红字凭证 + 库存回冲 + 入库单作废）
  与结算同属「一步不可逆」的动作，所以这里把两者收口成**同一套确认交互**：

  | 动作 | 先看到什么 | 还要做什么 |
  |---|---|---|
  | 结算 | 「将生成 N 张入库单 + 3 张凭证 + 金额合计」（`settlePlanText`） | 输入「结算」二字 |
  | 红冲 | 「红字凭证 + 回冲库存 + 入库单作废 + 批次置已红冲」（`reversePlanText`） | 原因 ≥10 字 + 输入「红冲」二字 |

  # 为什么必须输入关键字而不是「点两下确定」

  这两个动作都会动已入账的凭证与库存，误触成本是「会计要手工冲账」。仓库既有高危口径
  （收货免审过账、期间反结账）都是**关键字确认**：手要离开鼠标做一件与「下一步」无关的事，
  误点率降到可忽略。关键字比较用 `keywordMatches`（逐字全等、去首尾空白），
  不 trim 内部、不做模糊匹配。
-->
<template>
  <t-dialog
    v-model:visible="visibleModel"
    :header="mode === 'reverse' ? '红冲结算（高危）' : '结算（高危）'"
    :confirm-btn="confirmBtn"
    :cancel-btn="{ content: '取消', variant: 'outline' }"
    :close-on-overlay-click="false"
    :destroy-on-close="true"
    width="560px"
    data-testid="cp-risk-dialog"
    @confirm="handleConfirm"
    @closed="handleClosed"
  >
    <div class="risk">
      <t-alert :theme="mode === 'reverse' ? 'error' : 'warning'" class="risk__alert">
        <template #message>
          <span data-testid="cp-risk-plan">{{ plan }}</span>
        </template>
      </t-alert>

      <div v-if="mode === 'reverse'" class="risk__field">
        <div class="risk__label">
          红冲原因 <span class="risk__req">*</span>
          <span class="risk__hint">（至少 {{ REVERSE_REASON_MIN }} 个字，会写进红字凭证摘要与入库单作废原因）</span>
        </div>
        <t-textarea
          v-model="reason"
          :maxlength="255"
          :autosize="{ minRows: 2, maxRows: 4 }"
          placeholder="例如：10 月完工数量录错，冲回后按正确数量重算"
          data-testid="cp-risk-reason"
        />
        <div v-if="reasonHintText" class="risk__error" data-testid="cp-risk-reason-error">
          {{ reasonHintText }}
        </div>
      </div>

      <div class="risk__field">
        <div class="risk__label">
          请输入「<b>{{ keyword }}</b>」以继续
        </div>
        <t-input
          v-model="typed"
          :placeholder="keyword"
          data-testid="cp-risk-keyword"
          @enter="handleConfirm"
        />
        <div v-if="typed.length > 0 && !keywordOk" class="risk__error" data-testid="cp-risk-keyword-error">
          必须逐字输入「{{ keyword }}」（当前「{{ typed.trim() }}」）
        </div>
      </div>

      <p v-if="error" class="risk__server" data-testid="cp-risk-error">{{ error }}</p>
    </div>
  </t-dialog>
</template>

<script setup lang="ts">
/**
 * 结算 / 红冲确认框：本地只做「原因长度 + 关键字」两道闸，
 * 真正的业务拒绝（62077 断言不平 / 66078 库存退不回）由服务端报文原样展示。
 */
import { computed, ref, watch } from 'vue';

import { REVERSE_KEYWORD, REVERSE_REASON_MIN, SETTLE_KEYWORD } from '@/api/business/erp/cost-period-kinds';
import { keywordMatches, reasonHint } from '../cost-period-logic';

const props = withDefaults(
  defineProps<{
    /** 显示（v-model:visible） */
    visible: boolean;
    /** 动作：结算 / 红冲 */
    mode: 'settle' | 'reverse';
    /** 计划文案（`settlePlanText` / `reversePlanText`） */
    plan: string;
    /** 提交中 */
    loading?: boolean;
    /** 服务端错误（原样展示） */
    error?: string;
  }>(),
  { loading: false, error: '' },
);

const emit = defineEmits<{
  'update:visible': [value: boolean];
  /** 确认：结算时 reason 为空串；红冲时是 ≥10 字的原因 */
  confirm: [reason: string];
}>();

const visibleModel = computed<boolean>({
  get: () => props.visible,
  set: (value) => emit('update:visible', value),
});

/** 关键字：结算「结算」/ 红冲「红冲」 */
const keyword = computed<string>(() => (props.mode === 'reverse' ? REVERSE_KEYWORD : SETTLE_KEYWORD));

const typed = ref('');
const reason = ref('');

/** 每次打开都清空（否则上一次的输入会漏到下一张单） */
watch(
  () => props.visible,
  (open) => {
    if (open) {
      typed.value = '';
      reason.value = '';
    }
  },
);

const keywordOk = computed<boolean>(() => keywordMatches(typed.value, keyword.value));

/** 红冲原因提示（结算模式下恒 null） */
const reasonHintText = computed<string | null>(() =>
  props.mode === 'reverse' && reason.value.length > 0 ? reasonHint(reason.value) : null,
);

/** 确认按钮：两闸都过才可点（不让用户点出一个 400） */
const confirmBtn = computed(() => ({
  content: props.mode === 'reverse' ? '确认红冲' : '确认结算',
  theme: props.mode === 'reverse' ? ('danger' as const) : ('primary' as const),
  loading: props.loading,
  disabled: !keywordOk.value || (props.mode === 'reverse' && reasonHint(reason.value) !== null),
}));

function handleConfirm(): void {
  if (confirmBtn.value.disabled) {
    return;
  }
  emit('confirm', props.mode === 'reverse' ? reason.value.trim() : '');
}

/** 关闭后清空（下次打开走 watch，双保险） */
function handleClosed(): void {
  typed.value = '';
  reason.value = '';
}
</script>

<style scoped>
.risk {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.risk__alert {
  margin: 0;
}

.risk__field {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.risk__label {
  font-size: 13px;
  color: var(--td-text-color-primary);
}

.risk__req {
  color: var(--td-error-color);
}

.risk__hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.risk__error {
  font-size: 12px;
  color: var(--td-error-color);
}

.risk__server {
  margin: 0;
  padding: 8px 10px;
  font-size: 12px;
  line-height: 1.5;
  color: var(--td-error-color);
  background: var(--td-error-color-1);
  border-radius: 4px;
  white-space: pre-wrap;
}
</style>

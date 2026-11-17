<!--
  客户信用面板（M3-14）—— 额度 / 占用 / 可用，内嵌在客户详情（编辑弹窗）里。

  # 它回答三个问题

  1. **这家还能欠多少**（可用额度 + 占用率进度条）；
  2. **占用是怎么来的**（已占 / 在审 / 占用合计，加上对照的未清应收）；
  3. **本单会不会被拦**（判定 tag + 策略 + 后端 `basis` 口径说明）。

  # 数据全部来自后端，本组件不算钱

  `GET /api/business/erp/credit/{customer_id}`（M3-10）。理由见
  `./credit-panel-logic.ts` 文件头：第二份公式和后端 `credit::decide` 一旦分叉，
  屏幕写「额度内」而提交 409 —— 比不显示更糟。

  # 三条交互口径

  - **取不到就降级，不挡路**：客户详情是编辑表单，信用只是只读附加信息。
    接口失败（403 / 500 / 超时）只把面板换成一行提示 + 「重试」，**不弹窗、不禁用保存**。
    这一点与 `customer/index.vue` 头部「取不到就不显示胶囊」的既有约定一致；
  - **`null` 与 `0` 不同**：额度 0 = 不控额度（`available` 为 `null`），
    和「额度用完（0.00）」必须区分显示，见 `creditAvailableText`；
  - **超额只认后端 `is_over` / `over`**：`available` 已被后端夹到 0 以上，
    写 `available < 0` 永远不会成立。
-->
<template>
  <div class="credit-panel" :data-testid="`${prefix}-panel`">
    <!-- ① 加载中（首次；后续刷新保留旧值不闪） -->
    <div v-if="loading && !snapshot" class="credit-panel__state" :data-testid="`${prefix}-loading`">
      <t-loading size="small" />
      <span>正在计算信用占用…</span>
    </div>

    <!-- ② 取不到：降级提示 + 重试（不打断表单） -->
    <div
      v-else-if="errorText && !snapshot"
      class="credit-panel__state credit-panel__state--error"
      :data-testid="`${prefix}-error`"
    >
      <t-icon name="error-circle" />
      <span class="credit-panel__error-text" :title="errorText">{{ errorText }}</span>
      <t-link theme="primary" :data-testid="`${prefix}-retry`" @click="reload">重试</t-link>
    </div>

    <!-- ③ 有数据 -->
    <template v-else-if="snapshot">
      <!-- 冻结：比额度更硬的一道闸，单独提示 -->
      <t-alert
        v-if="frozenText"
        theme="warning"
        :message="frozenText"
        class="credit-panel__alert"
        :data-testid="`${prefix}-frozen`"
      />

      <div class="credit-panel__head">
        <div class="credit-panel__headline">
          <span class="credit-panel__amount" :class="headlineClass" :data-testid="`${prefix}-headline`">
            {{ headline }}
          </span>
          <t-tag
            size="small"
            variant="light"
            :theme="verdict.theme"
            :title="verdict.hint"
            :data-testid="`${prefix}-verdict`"
          >
            {{ verdict.label }}
          </t-tag>
          <span class="credit-panel__policy" :data-testid="`${prefix}-policy`">
            超限策略：{{ policyText }}
          </span>
        </div>
        <div class="credit-panel__ops">
          <t-tooltip :content="basis" placement="top-right">
            <t-icon name="help-circle" class="credit-panel__help" :data-testid="`${prefix}-basis-icon`" />
          </t-tooltip>
          <t-link theme="default" :disabled="loading" @click="reload">刷新</t-link>
        </div>
      </div>

      <!-- 占用率进度条（不控额度时不画条：没有分母） -->
      <div v-if="usagePercent !== null" class="credit-panel__usage">
        <t-progress
          theme="line"
          :percentage="progressPercent"
          :label="false"
          :status="progressStatus"
          :data-testid="`${prefix}-progress`"
        />
        <span class="credit-panel__usage-text" :data-testid="`${prefix}-usage`">{{ usageText }}</span>
      </div>
      <div v-else class="credit-panel__usage-text" :data-testid="`${prefix}-usage`">{{ usageText }}</div>
      <!-- 明细（额度 / 已占 / 在审 / 占用 / 可用 / 未清应收） -->
      <div class="credit-panel__rows" :data-testid="`${prefix}-rows`">
        <div
          v-for="row in rows"
          :key="row.key"
          class="credit-panel__row"
          :class="row.tone ? `credit-panel__row--${row.tone}` : undefined"
          :data-testid="`${prefix}-row-${row.key}`"
        >
          <span class="credit-panel__label">
            {{ row.label }}
            <t-tooltip v-if="row.hint" :content="row.hint" placement="top-left">
              <t-icon name="help-circle" class="credit-panel__help" />
            </t-tooltip>
          </span>
          <span class="credit-panel__value mono">{{ row.value }}</span>
        </div>
      </div>

      <!-- 缓存列漂移：运维信号，不影响本页显示（本页永远按现算值） -->
      <t-alert
        v-if="driftText"
        theme="warning"
        :message="driftText"
        class="credit-panel__alert"
        :data-testid="`${prefix}-drift`"
      />

      <p class="credit-panel__basis" :data-testid="`${prefix}-basis`">{{ basis }}</p>
    </template>
  </div>
</template>

<script setup lang="ts">
/**
 * 客户信用面板（M3-14）。
 *
 * 组件自带加载：调用方只给 `customerId`（+ 可选的 `amount` 试算），
 * 不需要在父页面里再写一遍 loading / error 状态机 —— 客户详情页将来多处用它
 * （编辑弹窗、详情抽屉、未来的客户 360 视图），把取数留在组件里只维护一处。
 */
import { computed, onMounted, ref, watch } from 'vue';
import { ApiError } from '@/api/request';
import { fetchCreditSnapshot, type CreditSnapshot, type DecimalText } from '@/api/business/erp/credit';
import {
  creditBasisText,
  creditDriftText,
  creditFrozenText,
  creditHeadlineText,
  creditPanelRows,
  creditPolicyText,
  creditProgressPercent,
  creditProgressStatus,
  creditUsagePercent,
  creditUsageText,
  creditVerdictMeta,
  isUncontrolled,
} from './credit-panel-logic';

const props = withDefaults(
  defineProps<{
    /** 客户主键（UUID） */
    customerId: string;
    /** 本次订单价税合计（试算；不传 = 只看当前口径） */
    amount?: DecimalText | null;
    /** 从「在审」里排除本单（编辑已提交单时用） */
    excludeOrderId?: string | null;
    /** `data-testid` 前缀（默认 `cus-credit`；同页多个面板时区分用） */
    testidPrefix?: string;
    /**
     * 外部刷新信号（父页面打开详情 / 保存成功后 `+1`）。
     *
     * 必须有：TDesign 的弹窗关闭后**不销毁内容**（实测：关掉再打开同一家客户，
     * 面板实例还在、`customerId` 也没变 ⇒ watch 不触发 ⇒ 用户刚把额度从 5 万改成
     * 12 万，再打开还看到「已超 1 万」的旧数）。父页面在每次打开/保存后拨一格。
     */
    refreshKey?: number;
  }>(),
  { amount: null, excludeOrderId: null, testidPrefix: 'cus-credit', refreshKey: 0 },
);

const emit = defineEmits<{
  /** 取到快照（父页面需要联动时用，例如在订单表单里回填可用额度） */
  loaded: [snapshot: CreditSnapshot];
  /** 取数失败（父页面可忽略：面板自己已经降级显示） */
  failed: [message: string];
}>();

const prefix = computed(() => props.testidPrefix);
const snapshot = ref<CreditSnapshot | null>(null);
const loading = ref(false);
const errorText = ref('');

/**
 * 当前快照属于哪个客户（**换客户必须清旧数**）。
 *
 * 实测踩到的坑：编辑弹窗关掉再打开另一家客户时，TDesign 的弹窗内容可能复用同一个
 * 面板实例 —— 若请求失败还「保留上一次的数」，B 的表头下就会显示 A 的额度（冻结
 * 条、已占、可用全是别人的）。所以「同一客户刷新失败」才保留旧值，
 * 「换了客户」一律先清空，宁可显示「取不到」也不显示别人的钱。
 */
const snapshotCustomerId = ref('');

/**
 * 请求序号：`customerId` / `amount` / `refreshKey` 任一变化都会新发一次请求，
 * 只有**最后发出**的那次结果被采纳 —— 否则快速连点「刷新」会被先到的旧响应覆盖。
 */
let seq = 0;

/** 错误 → 人话（`ApiError` 带后端 msg，别吃掉） */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/** 取一次快照（失败只记文案，面板降级；不抛、不弹窗） */
async function reload(): Promise<void> {
  const customerId = props.customerId;
  if (!customerId) {
    snapshot.value = null;
    snapshotCustomerId.value = '';
    errorText.value = '';
    return;
  }
  // 换了客户：旧快照是别人的数据，立刻丢掉（见 `snapshotCustomerId` 的说明）
  if (snapshotCustomerId.value !== customerId) {
    snapshot.value = null;
    snapshotCustomerId.value = '';
  }
  const mine = ++seq;
  loading.value = true;
  errorText.value = '';
  try {
    const data = await fetchCreditSnapshot(customerId, {
      amount: props.amount,
      exclude_order_id: props.excludeOrderId,
    });
    if (mine !== seq) {
      return;
    }
    snapshot.value = data;
    snapshotCustomerId.value = customerId;
    emit('loaded', data);
  } catch (error) {
    if (mine !== seq) {
      return;
    }
    // 同一客户刷新失败：保留上一次的数（屏幕不该被清空）；没取到过才显示错误块
    errorText.value = `信用数据取不到：${messageOf(error)}`;
    emit('failed', errorText.value);
  } finally {
    if (mine === seq) {
      loading.value = false;
    }
  }
}

onMounted(() => {
  void reload();
});

watch(
  () => [props.customerId, props.amount, props.excludeOrderId, props.refreshKey],
  () => {
    void reload();
  },
);

// ==================== 展示派生 ====================

const verdict = computed(() => creditVerdictMeta(snapshot.value?.verdict));
const policyText = computed(() => creditPolicyText(snapshot.value?.credit_policy));
const headline = computed(() => (snapshot.value ? creditHeadlineText(snapshot.value) : ''));
const usagePercent = computed(() => (snapshot.value ? creditUsagePercent(snapshot.value) : null));
const progressPercent = computed(() => (snapshot.value ? creditProgressPercent(snapshot.value) : 0));
const progressStatus = computed(() => (snapshot.value ? creditProgressStatus(snapshot.value) : undefined));
const usageText = computed(() => (snapshot.value ? creditUsageText(snapshot.value) : ''));
const rows = computed(() => (snapshot.value ? creditPanelRows(snapshot.value) : []));
const basis = computed(() => creditBasisText(snapshot.value));
const driftText = computed(() => (snapshot.value ? creditDriftText(snapshot.value) : ''));
const frozenText = computed(() => (snapshot.value ? creditFrozenText(snapshot.value) : ''));

/** 顶部一句话的配色：超额红、接近额度黄、不控额度灰、正常绿 */
const headlineClass = computed(() => {
  if (!snapshot.value) {
    return undefined;
  }
  if (snapshot.value.is_over) {
    return 'credit-panel__amount--over';
  }
  if (isUncontrolled(snapshot.value)) {
    return 'credit-panel__amount--muted';
  }
  const percent = usagePercent.value;
  if (percent !== null && percent >= 80) {
    return 'credit-panel__amount--warn';
  }
  return 'credit-panel__amount--ok';
});
</script>

<style scoped lang="less">
.credit-panel {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 12px;
  border: 1px solid var(--td-component-stroke);
  border-radius: var(--td-radius-default);
  background: var(--td-bg-color-container);
}

.credit-panel__state {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.credit-panel__state--error {
  color: var(--td-error-color);
}

.credit-panel__error-text {
  flex: 1 1 auto;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.credit-panel__alert {
  margin: 0;
}

.credit-panel__head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-wrap: wrap;
}

.credit-panel__headline {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.credit-panel__amount {
  font-size: 16px;
  font-weight: 600;
}

.credit-panel__amount--ok {
  color: var(--td-success-color);
}

.credit-panel__amount--warn {
  color: var(--td-warning-color);
}

.credit-panel__amount--over {
  color: var(--td-error-color);
}

.credit-panel__amount--muted {
  color: var(--td-text-color-secondary);
}

.credit-panel__policy {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.credit-panel__ops {
  display: flex;
  align-items: center;
  gap: 10px;
}

.credit-panel__help {
  color: var(--td-text-color-placeholder);
  cursor: help;
}

.credit-panel__usage {
  display: flex;
  flex-direction: column;
  align-items: stretch;
  gap: 2px;
}

.credit-panel__usage :deep(.t-progress) {
  width: 100%;
}

.credit-panel__usage-text {
  font-size: 12px;
  color: var(--td-text-color-secondary);
  font-variant-numeric: tabular-nums;
}

.credit-panel__rows {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 4px 16px;
}

.credit-panel__row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  font-size: 13px;
  padding: 2px 0;
  border-bottom: 1px dashed var(--td-component-stroke);
}

.credit-panel__label {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  color: var(--td-text-color-secondary);
}

.credit-panel__value {
  font-variant-numeric: tabular-nums;
}

.credit-panel__row--strong .credit-panel__value {
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.credit-panel__row--muted .credit-panel__value {
  color: var(--td-text-color-placeholder);
}

.credit-panel__row--danger .credit-panel__value {
  color: var(--td-error-color);
  font-weight: 600;
}

.credit-panel__basis {
  margin: 0;
  font-size: 12px;
  line-height: 1.5;
  color: var(--td-text-color-placeholder);
}

.mono {
  font-family: var(--td-font-family-number, monospace);
}
</style>

<template>
  <div class="variance-panel">
    <div class="filter-row">
      <t-input
        v-model="periodInput"
        class="filter-row__period"
        placeholder="期间：2026-11 或 2026-Q4"
        data-testid="variance-period"
        @enter="load"
      />
      <t-select
        v-model="dimension"
        class="filter-row__dimension"
        :options="DIMENSION_OPTIONS"
        data-testid="variance-dimension"
        @change="load"
      />
      <t-input
        v-model="toleranceInput"
        class="filter-row__tolerance"
        placeholder="容差 %（默认 5）"
        data-testid="variance-tolerance"
        @enter="load"
      />
      <t-button theme="primary" :loading="loading" data-testid="btn-variance-search" @click="load">
        对比
      </t-button>
      <span class="filter-row__spacer" />
      <t-tag variant="light" theme="default" data-testid="variance-basis-tip">
        实际口径：有效订单本位币含税金额；预测只取当前版本
      </t-tag>
    </div>

    <t-alert
      v-if="error"
      theme="error"
      :message="error"
      class="page-alert"
      data-testid="variance-error"
    />

    <template v-if="data">
      <div class="headline-row">
        <span class="headline-row__grain">
          {{ data.period_code }} · {{ data.dimension_label }} · 容差 ±{{ toleranceText(data.tolerance_pct) }}%
        </span>
        <span class="headline-row__text" data-testid="variance-headline">{{ headline }}</span>
      </div>

      <div class="summary-row">
        <t-card :bordered="true" class="summary-card">
          <div class="summary-card__label">预测合计（当前版本）</div>
          <div class="summary-card__value mono" data-testid="variance-forecast-total">
            {{ formatMoney(data.summary.forecast_amount) }}
          </div>
          <div class="summary-card__hint">{{ data.summary.forecast_count }} 行有预测</div>
        </t-card>
        <t-card :bordered="true" class="summary-card">
          <div class="summary-card__label">实际合计（订单口径）</div>
          <div class="summary-card__value mono" data-testid="variance-actual-total">
            {{ formatMoney(data.summary.actual_amount) }}
          </div>
          <div class="summary-card__hint">含漏预测行的实际额</div>
        </t-card>
        <t-card :bordered="true" class="summary-card">
          <div class="summary-card__label">偏差（实际 − 预测）</div>
          <div
            class="summary-card__value mono"
            :class="toneClass(data.summary.variance_amount)"
            data-testid="variance-total-diff"
          >
            {{ formatMoney(data.summary.variance_amount) }}
          </div>
          <div class="summary-card__hint">
            合计偏差率 <span class="mono" data-testid="variance-total-rate">{{ rateText(data.summary.variance_rate) }}</span>
            （= Σ实际 ÷ Σ预测，不是行率平均）
          </div>
        </t-card>
        <t-card :bordered="true" class="summary-card">
          <div class="summary-card__label">平均绝对偏差率 MAPE</div>
          <div class="summary-card__value mono" data-testid="variance-mape">{{ rateText(data.summary.mape_pct) }}</div>
          <div class="summary-card__hint">只算有预测金额的行</div>
        </t-card>
      </div>

      <t-alert
        v-if="worstLine"
        theme="warning"
        :message="`偏差最贵的一行：${worstLine}`"
        class="page-alert"
        data-testid="variance-worst"
      />

      <t-alert
        v-if="otherGrainHint"
        theme="info"
        :message="otherGrainHint"
        class="page-alert"
        data-testid="variance-dimension-hint"
      />

      <t-loading :loading="loading" size="small">
        <div v-if="chartOption" class="chart-block">
          <EChart :option="chartOption" :height="340" data-testid="variance-chart" />
          <p v-if="chartCutText" class="chart-block__note" data-testid="variance-chart-note">{{ chartCutText }}</p>
        </div>
        <div v-else class="chart-empty" data-testid="variance-chart-empty">
          <t-icon name="chart-bar" class="chart-empty__icon" />
          <p>这个期间 / 粒度下没有任何预测或订单 —— 换个期间或先录入预测</p>
        </div>
      </t-loading>

      <t-table
        row-key="display_name"
        size="small"
        hover
        :data="data.rows"
        :columns="columns"
        :loading="loading"
        :empty="'没有可对比的行'"
        data-testid="variance-table"
      >
        <template #display_name="{ row }">
          <span class="name-cell" :title="row.display_name">{{ row.display_name }}</span>
        </template>
        <template #forecast_amount="{ row }">
          <span class="mono">{{ formatMoney(row.forecast_amount) }}</span>
        </template>
        <template #actual_amount="{ row }">
          <span class="mono">{{ formatMoney(row.actual_amount) }}</span>
        </template>
        <template #variance_amount="{ row }">
          <span class="mono" :class="toneClass(row.variance_amount)" data-testid="variance-row-diff">
            {{ formatMoney(row.variance_amount) }}
          </span>
        </template>
        <template #variance_rate="{ row }">
          <span class="mono" data-testid="variance-row-rate">{{ rateText(row.variance_rate) }}</span>
        </template>
        <template #qty="{ row }">
          <span class="mono">
            {{ qtyText(row.forecast_qty) }} / {{ qtyText(row.actual_qty) }}
            <span class="muted">（{{ qtyText(row.variance_qty) }}）</span>
          </span>
        </template>
        <template #deviation_level="{ row }">
          <t-tag
            size="small"
            variant="light"
            :theme="deviationMeta(row.deviation_level).theme"
            data-testid="variance-row-level"
          >
            {{ deviationMeta(row.deviation_level).label }}
          </t-tag>
        </template>
        <template #source="{ row }">
          <span v-if="row.has_forecast" class="mono source-cell">
            {{ row.forecast_no }}<span class="muted"> v{{ row.version_no }}</span>
          </span>
          <span v-else class="muted">无预测行</span>
        </template>
      </t-table>
      <p class="table-note">
        数量列：预测 / 实际（括号内为差额，实际 − 预测）。偏差率 `—` 表示预测侧为 0，比率没有意义（不是 0%）。
      </p>
    </template>

    <div v-else-if="loading" class="chart-empty">
      <t-loading :loading="true" size="small" text="加载中…" />
    </div>
  </div>
</template>

<script setup lang="ts">
/**
 * 「预测 vs 实际」对比页签（M3-23 前端页面，数据来自 M3-21 的
 * `GET /api/business/erp/sales-forecast/variance`）。
 *
 * # 三条硬约束（都有单测，改之前先看 `forecast-variance-logic.ts` 的头注释）
 *
 * 1. **汇总只展示后端 `summary`**，前端不重算 —— 后端 `summarize_variance` 与明细同源，
 *    前端再算一遍必然某天口径漂移（M3-22 专门钉过「行相加 = summary」）；
 * 2. **偏差率 `null` 显示 `—`**（预测为 0 ⇒ 比率无意义），不是 `0.00%`；
 * 3. **图只画前 N 行**（默认 12，按 |偏差金额| 倒序 = 后端行序），表格是全量；
 *    图与表顺序一致，别在前端重排。
 *
 * # 权限
 *
 * 与预测列表同一个码 `business:erp:sales-forecast:list`（M3-21 决定，零新增权限码），
 * 所以本页签不再单独 `v-privilege`；没码时后端 403，错误原文摆在页里。
 */
import { computed, onMounted, ref } from 'vue';
import EChart from '@/components/EChart.vue';
import { ApiError } from '@/api/request';
import {
  fetchForecastVariance,
  formatMoney,
  type ForecastDimension,
  type ForecastVarianceRes,
} from '@/api/business/erp/sales-target';
import {
  CHART_ROW_LIMIT,
  DIMENSION_OPTIONS,
  buildVarianceChartOption,
  deviationMeta,
  dimensionHint,
  qtyText,
  rateText,
  summaryHeadline,
  toleranceText,
  varianceTone,
  worstText,
} from './forecast-variance-logic';

const props = withDefaults(defineProps<{ initPeriod?: string }>(), { initPeriod: '' });

/** 默认期间：本月（北京时间口径够用，后端会归一化） */
function currentPeriod(): string {
  const now = new Date();
  return `${now.getFullYear()}-${`${now.getMonth() + 1}`.padStart(2, '0')}`;
}

const periodInput = ref(props.initPeriod || currentPeriod());
const dimension = ref<ForecastDimension>('CUSTOMER');
const toleranceInput = ref('5');
const loading = ref(false);
const error = ref('');
const data = ref<ForecastVarianceRes | null>(null);

const columns = [
  { colKey: 'display_name', title: '客户 / 物料（对比粒度）', minWidth: 200 },
  { colKey: 'forecast_amount', title: '预测金额', width: 135, align: 'right' as const },
  { colKey: 'actual_amount', title: '实际金额', width: 135, align: 'right' as const },
  { colKey: 'variance_amount', title: '偏差（实际−预测）', width: 150, align: 'right' as const },
  { colKey: 'variance_rate', title: '偏差率', width: 100, align: 'right' as const },
  { colKey: 'qty', title: '数量（预测 / 实际）', width: 190, align: 'right' as const },
  { colKey: 'deviation_level', title: '档位', width: 100 },
  { colKey: 'source', title: '预测单 / 版本', width: 170 },
];

const headline = computed(() => (data.value ? summaryHeadline(data.value.summary) : ''));
const worstLine = computed(() => worstText(data.value?.summary.worst ?? null));
const otherGrainHint = computed(() =>
  data.value ? dimensionHint(data.value.dimension_counts, data.value.dimension) : '',
);

const chartOption = computed(() =>
  data.value && data.value.rows.length > 0 ? buildVarianceChartOption(data.value, CHART_ROW_LIMIT) : null,
);

const chartCutText = computed(() => {
  const res = data.value;
  if (!res || res.rows.length <= CHART_ROW_LIMIT) {
    return '';
  }
  return `图里只画偏差最大的 ${CHART_ROW_LIMIT} 行（按 |偏差金额| 倒序），表格是全量 ${res.rows.length} 行`;
});

/** 偏差方向配色：正 = 实际超出（红，超卖/超收要人看），负 = 实际低于（绿，没达成） */
function toneClass(value: unknown): string {
  const tone = varianceTone(value as string | number | null | undefined);
  if (tone === 'over') {
    return 'diff-over';
  }
  return tone === 'under' ? 'diff-under' : '';
}

/** 错误 → 人话（ApiError 带后端 msg，别吃掉） */
function messageOf(value: unknown): string {
  if (value instanceof ApiError) {
    return value.message;
  }
  return value instanceof Error ? value.message : String(value);
}

/** 容差入参校验：空 = 用后端缺省 5；否则 0~100 且最多 2 位小数（与后端 400 同口径） */
function validateTolerance(raw: string): string {
  const text = raw.trim();
  if (text === '') {
    return '';
  }
  if (!/^\d{1,3}(\.\d{1,2})?$/.test(text)) {
    return `容差只认 0 ~ 100 的数字，最多 2 位小数（收到「${raw}」；5 = ±5%）`;
  }
  const num = Number(text);
  if (!(num >= 0 && num <= 100)) {
    return `容差必须在 0 ~ 100 之间（收到 ${text}；这是百分数，5 = ±5%）`;
  }
  return '';
}

async function load(): Promise<void> {
  const period = periodInput.value.trim();
  if (!/^\d{4}-(0[1-9]|1[0-2]|[Qq][1-4])$/.test(period)) {
    // 参数不合法：只报错，**不清掉上一次的结果**（输错一个字符不该让整页空掉）
    error.value = '期间码只认 yyyy-MM 或 yyyy-QN（如 2026-11 / 2026-Q4）';
    return;
  }
  const toleranceInvalid = validateTolerance(toleranceInput.value);
  if (toleranceInvalid) {
    error.value = toleranceInvalid;
    return;
  }
  loading.value = true;
  error.value = '';
  try {
    data.value = await fetchForecastVariance({
      period_code: period.toUpperCase(),
      dimension: dimension.value,
      tolerance_pct: toleranceInput.value.trim() || undefined,
    });
  } catch (caught) {
    data.value = null;
    error.value = `对比加载失败：${messageOf(caught)}`;
  } finally {
    loading.value = false;
  }
}

onMounted(() => {
  void load();
});
</script>

<style scoped>
.variance-panel {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.filter-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin-bottom: 8px;
}

.filter-row__period {
  width: 200px;
}

.filter-row__dimension {
  width: 170px;
}

.filter-row__tolerance {
  width: 150px;
}

.filter-row__spacer {
  flex: 1;
}

.page-alert {
  margin-bottom: 8px;
}

.headline-row {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 12px;
  margin-bottom: 8px;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.headline-row__grain {
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.summary-row {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  margin-bottom: 8px;
}

.summary-card {
  flex: 1 1 200px;
}

.summary-card__label {
  font-size: 12px;
  color: var(--td-text-color-secondary);
  margin-bottom: 6px;
}

.summary-card__value {
  font-size: 18px;
  font-weight: 600;
}

.summary-card__hint {
  margin-top: 4px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.chart-block {
  margin: 8px 0 12px;
}

.chart-block__note,
.table-note {
  margin-top: 6px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
  line-height: 1.7;
}

.chart-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  padding: 40px 0;
  color: var(--td-text-color-placeholder);
  font-size: 13px;
}

.chart-empty__icon {
  font-size: 28px;
}

.name-cell {
  display: inline-block;
  max-width: 260px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  vertical-align: bottom;
}

.source-cell {
  font-size: 12px;
}

.diff-over {
  color: var(--td-error-color);
}

.diff-under {
  color: var(--td-success-color);
}

.mono {
  font-family: var(--td-font-family-mono, monospace);
}

.muted {
  color: var(--td-text-color-placeholder);
}
</style>

<template>
  <div class="fxb-page">
    <t-card :bordered="false" class="fxb-card">
      <!-- ==================== 工具栏：期间 + 币种 + 查询 ==================== -->
      <div class="tool-row">
        <span class="tool-row__label">会计期间</span>
        <t-select
          v-model="period"
          class="tool-row__period"
          :options="periodOptions"
          :loading="periodLoading"
          filterable
          placeholder="必选（yyyy-MM）"
          data-testid="fxb-period-select"
          @change="handlePeriodChange"
        />
        <span class="tool-row__label">币种</span>
        <t-select
          v-model="currency"
          class="tool-row__currency"
          :options="currencyOptions"
          clearable
          filterable
          placeholder="全部外币"
          data-testid="fxb-currency-select"
          @change="load()"
        />
        <t-button theme="primary" :loading="loading" data-testid="fxb-search" @click="load()">查询</t-button>
      </div>

      <t-alert v-if="error" theme="error" :message="error" class="fxb-alert" data-testid="fxb-error">
        <template #operation>
          <t-button size="small" variant="outline" @click="load()">重试</t-button>
        </template>
      </t-alert>

      <!-- ==================== 表头 ==================== -->
      <div v-if="report" class="fxb-head" data-testid="fxb-head">
        <h2 class="fxb-head__title">{{ report.report_name }}</h2>
        <div class="fxb-head__meta">
          <span data-testid="fxb-company">编制单位：{{ report.company_name }}</span>
          <span data-testid="fxb-period-text">
            期间：{{ report.period }}（{{ periodStatusText(report.period_status) }}）
          </span>
          <span data-testid="fxb-period-range">{{ report.period_start }} ~ {{ report.period_end }}</span>
          <span data-testid="fxb-base-currency">本位币：{{ report.base_currency }}</span>
          <span data-testid="fxb-generated-at">编制时间：{{ formatDateTime(report.generated_at) }}</span>
        </div>
      </div>

      <!-- ==================== 关键指标 ==================== -->
      <div v-if="report" class="fxb-kpi" data-testid="fxb-kpi">
        <div class="fxb-kpi__item">
          <span class="fxb-kpi__label">外币币种 / 科目行</span>
          <b class="fxb-kpi__value" data-testid="fxb-counts">
            {{ report.currencies.length }} 种 / {{ report.rows.length }} 行
          </b>
        </div>
        <div class="fxb-kpi__item">
          <span class="fxb-kpi__label">期末账面合计（{{ report.base_currency }}）</span>
          <b class="fxb-kpi__value" data-testid="fxb-total-book">{{ formatAmount(report.total_closing_book) }}</b>
        </div>
        <div class="fxb-kpi__item">
          <span class="fxb-kpi__label">折算合计（期末即期）</span>
          <b class="fxb-kpi__value" data-testid="fxb-total-converted">{{ convertedTotalText(report) }}</b>
        </div>
        <div class="fxb-kpi__item">
          <span class="fxb-kpi__label">汇兑差额合计（未入账）</span>
          <b
            class="fxb-kpi__value"
            :class="{ 'fxb-kpi__value--warn': report.total_reval_diff !== null && sideOf(report.total_reval_diff) !== 'ZERO' }"
            data-testid="fxb-total-diff"
          >
            {{ diffTotalText(report) }}
          </b>
        </div>
      </div>

      <!-- ==================== 缺牌价提示 ==================== -->
      <t-alert
        v-if="missingRateMessage"
        theme="warning"
        :message="missingRateMessage"
        class="fxb-alert"
        data-testid="fxb-missing-rate"
      />

      <!-- ==================== 按币种汇总 ==================== -->
      <div v-if="report && report.currencies.length" class="fxb-section" data-testid="fxb-summary">
        <div class="fxb-section__title">按币种汇总</div>
        <div class="fxb-body">
          <table class="fxb-table">
            <thead>
              <tr>
                <th class="fxb-table__name">币种</th>
                <th class="fxb-table__num">科目行</th>
                <th class="fxb-table__num">期末原币余额</th>
                <th class="fxb-table__num">期末账面({{ report.base_currency }})</th>
                <th class="fxb-table__num">期末即期汇率</th>
                <th class="fxb-table__num">折算({{ report.base_currency }})</th>
                <th class="fxb-table__num fxb-table__num--total">汇兑差额</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="(row, index) in report.currencies"
                :key="row.currency_code"
                class="fxb-row"
                :data-testid="`fxb-summary-row-${index}`"
              >
                <td class="fxb-table__name">{{ currencyLabel(row.currency_code, row.currency_name) }}</td>
                <td class="fxb-table__num">{{ row.account_count }}</td>
                <td class="fxb-table__num">{{ balanceText(row.closing_fc) }}</td>
                <td class="fxb-table__num">{{ balanceText(row.closing_book) }}</td>
                <td class="fxb-table__num">{{ rateText(row.close_rate) }}</td>
                <td class="fxb-table__num">{{ amountCell(row.converted_base) }}</td>
                <td class="fxb-table__num fxb-table__num--total">{{ diffText(row.reval_diff) }}</td>
              </tr>
              <tr class="fxb-row fxb-row--total" data-testid="fxb-summary-total">
                <td class="fxb-table__name">合计（{{ report.currencies.length }} 种）</td>
                <td class="fxb-table__num">{{ report.rows.length }}</td>
                <td class="fxb-table__num fxb-table__muted">原币跨币种不相加</td>
                <td class="fxb-table__num">{{ formatAmount(report.total_closing_book) }}</td>
                <td class="fxb-table__num fxb-table__muted">—</td>
                <td class="fxb-table__num">{{ convertedTotalText(report) }}</td>
                <td class="fxb-table__num fxb-table__num--total">{{ diffTotalText(report) }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>

      <!-- ==================== 明细：按科目 × 币种 ==================== -->
      <div v-if="report" class="fxb-section" data-testid="fxb-table">
        <div class="fxb-section__title">
          科目明细（{{ report.rows.length }} 行<template v-if="report.currency_filter">，币种 {{ report.currency_filter }}</template>）
        </div>
        <t-loading :loading="loading" size="small">
          <div class="fxb-body">
            <table class="fxb-table">
              <thead>
                <tr>
                  <th class="fxb-table__name">科目</th>
                  <th class="fxb-table__name">币种</th>
                  <th class="fxb-table__num">期初原币</th>
                  <th class="fxb-table__num">本期借(原币)</th>
                  <th class="fxb-table__num">本期贷(原币)</th>
                  <th class="fxb-table__num">期末原币</th>
                  <th class="fxb-table__num">期初账面</th>
                  <th class="fxb-table__num">本期借(账面)</th>
                  <th class="fxb-table__num">本期贷(账面)</th>
                  <th class="fxb-table__num">期末账面</th>
                  <th class="fxb-table__num">期末即期汇率</th>
                  <th class="fxb-table__num">折算({{ report.base_currency }})</th>
                  <th class="fxb-table__num fxb-table__num--total">汇兑差额</th>
                  <th class="fxb-table__name">账面口径</th>
                </tr>
              </thead>
              <tbody>
                <tr
                  v-for="(row, index) in report.rows"
                  :key="`${row.account_id}-${row.currency_code}`"
                  class="fxb-row"
                  :data-testid="`fxb-row-${index}`"
                >
                  <td class="fxb-table__name" :data-testid="`fxb-row-account-${index}`">
                    {{ row.account_code }} {{ row.account_name }}
                  </td>
                  <td class="fxb-table__name">{{ currencyLabel(row.currency_code, row.currency_name) }}</td>
                  <td class="fxb-table__num">{{ balanceText(row.opening_fc) }}</td>
                  <td class="fxb-table__num">{{ formatAmount(row.period_fc_debit) }}</td>
                  <td class="fxb-table__num">{{ formatAmount(row.period_fc_credit) }}</td>
                  <td class="fxb-table__num fxb-table__num--total">{{ balanceText(row.closing_fc) }}</td>
                  <td class="fxb-table__num">{{ balanceText(row.opening_book) }}</td>
                  <td class="fxb-table__num">{{ formatAmount(row.period_book_debit) }}</td>
                  <td class="fxb-table__num">{{ formatAmount(row.period_book_credit) }}</td>
                  <td class="fxb-table__num fxb-table__num--total">{{ balanceText(row.closing_book) }}</td>
                  <td class="fxb-table__num">{{ rateText(row.close_rate) }}</td>
                  <td class="fxb-table__num" :data-testid="`fxb-row-converted-${index}`">
                    {{ amountCell(row.converted_base) }}
                  </td>
                  <td
                    class="fxb-table__num fxb-table__num--total"
                    :class="{ 'fxb-cell--warn': row.reval_diff !== null && sideOf(row.reval_diff) !== 'ZERO' }"
                    :data-testid="`fxb-row-diff-${index}`"
                  >
                    {{ diffText(row.reval_diff) }}
                  </td>
                  <td class="fxb-table__name">
                    <t-tooltip :content="basisHint(row.book_basis)">
                      <span :class="{ 'fxb-cell--basis-warn': row.book_basis === 'CURRENCY' }">
                        {{ basisLabel(row.book_basis) }}
                      </span>
                    </t-tooltip>
                  </td>
                </tr>
                <tr v-if="!report.rows.length">
                  <td class="fxb-empty" colspan="14" data-testid="fxb-empty">
                    {{ emptyText(report, report.base_currency) }}
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </t-loading>
      </div>

      <!-- ==================== 数据质量问题 ==================== -->
      <div v-if="report && report.warnings.length" class="fxb-warnings" data-testid="fxb-warnings">
        <div class="fxb-warnings__title">需要人工处理</div>
        <ul>
          <li v-for="(warning, index) in report.warnings" :key="index">{{ warning }}</li>
        </ul>
      </div>

      <!-- ==================== 口径提示 ==================== -->
      <ul v-if="report && report.hints.length" class="fxb-hints" data-testid="fxb-hints">
        <li v-for="(hint, index) in report.hints" :key="index">{{ hint }}</li>
      </ul>

      <div v-if="!report && !loading && !error" class="fxb-tip" data-testid="fxb-tip">
        选择会计期间（可再选币种）后点「查询」，编外币余额表
      </div>
    </t-card>
  </div>
</template>

<script setup lang="ts">
/**
 * 外币余额表（M1-07 · 迁移 `V0412`）。
 *
 * # 唯一口径：后端 `/business/erp/report/fx-balance`
 *
 * 页面**不算账**：期初 / 本期 / 期末、折算、汇兑差额、按币种汇总全部由后端算好
 * （取数口径与期末调汇 M1-05 逐字一致）。前端只做三件事：
 * 1. 把十进制**字符串**金额格式化成千分位（`fx-balance-logic.ts`，不过 `Number()`）；
 * 2. 按 `null` 显示占位符 —— 缺牌价的折算列**绝不能**画成 0；
 * 3. 把后端给的 `warnings` / `hints` 原样呈现（口径说明不写在前端，避免两处漂移）。
 */
import { computed, onMounted, ref } from 'vue';

import { fetchPeriodPage, type AccountPeriodRow } from '@/api/business/erp';
import { fetchCurrencyOptions, type CurrencyOption } from '@/api/business/erp/currency';
import {
  amountCell,
  balanceText,
  basisHint,
  basisLabel,
  convertedTotalText,
  currencyLabel,
  diffText,
  diffTotalText,
  emptyText,
  formatAmount,
  missingRateText,
  periodStatusText,
  rateText,
  sideOf,
} from './fx-balance-logic';
import { fetchFxBalance, type FxBalanceReport } from '@/api/business/erp/fx-balance';

const period = ref('');
const periodOptions = ref<{ label: string; value: string }[]>([]);
const periodLoading = ref(false);
const currency = ref('');
const currencyOptions = ref<{ label: string; value: string }[]>([]);
const loading = ref(false);
const error = ref('');
const report = ref<FxBalanceReport | null>(null);

const missingRateMessage = computed(() => (report.value ? missingRateText(report.value) : null));

/** 时间戳格式化（与其它报表页同一份实现，页面只读） */
function formatDateTime(value: string): string {
  if (!value) return '';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

/** 会计期间下拉：只列已建立的期间，按期间倒序（未建账的月份后端也能出表，但不在下拉里） */
async function loadPeriods() {
  periodLoading.value = true;
  try {
    const data = await fetchPeriodPage({ page: 1, page_size: 100 });
    const list = (data.list as AccountPeriodRow[])
      .filter((row) => row.status !== 'NOT_OPENED')
      .sort((a, b) => b.period_code.localeCompare(a.period_code));
    periodOptions.value = list.map((row) => ({
      label: `${row.period_code}（${row.status === 'CLOSED' ? '已结账' : '未结账'}）`,
      value: row.period_code,
    }));
    if (!period.value && list.length) {
      period.value = list[0].period_code;
    }
  } catch (err) {
    // 期间列表拉不到不致命：可以手填期间（可搜索下拉）
    error.value = `期间列表加载失败：${(err as Error).message ?? err}`;
  } finally {
    periodLoading.value = false;
  }
}

/** 币种下拉：只列启用的外币（本位币不构成外币余额，选中它只会得到空表） */
async function loadCurrencies() {
  try {
    const list = await fetchCurrencyOptions();
    currencyOptions.value = (list as CurrencyOption[])
      .filter((item) => !item.is_base)
      .map((item) => ({ label: item.label, value: item.code }));
  } catch {
    // 币种列表拉不到只影响筛选体验，不挡查询
  }
}

async function load() {
  if (!period.value) {
    error.value = '请先选择会计期间';
    return;
  }
  loading.value = true;
  error.value = '';
  try {
    report.value = await fetchFxBalance({
      period: period.value,
      currency: currency.value || undefined,
    });
  } catch (err) {
    report.value = null;
    error.value = (err as Error).message ?? String(err);
  } finally {
    loading.value = false;
  }
}

/** 切期间 → 期间变了币种过滤保留，直接重算 */
function handlePeriodChange() {
  void load();
}

onMounted(async () => {
  await Promise.all([loadPeriods(), loadCurrencies()]);
  await load();
});
</script>

<style scoped>
.fxb-card {
  min-height: 200px;
}

.fxb-alert {
  margin-bottom: 12px;
}

.fxb-head {
  text-align: center;
  margin: 8px 0 12px;
}

.fxb-head__title {
  margin: 0 0 6px;
  font-size: 20px;
  font-weight: 600;
  letter-spacing: 2px;
}

.fxb-head__meta {
  display: flex;
  justify-content: center;
  gap: 20px;
  flex-wrap: wrap;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.fxb-kpi {
  display: flex;
  gap: 12px;
  flex-wrap: wrap;
  margin-bottom: 12px;
}

.fxb-kpi__item {
  flex: 1 1 200px;
  padding: 10px 14px;
  border-radius: 6px;
  background: var(--td-bg-color-secondarycontainer);
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.fxb-kpi__label {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.fxb-kpi__value {
  font-size: 18px;
  font-weight: 600;
  font-variant-numeric: tabular-nums;
}

.fxb-kpi__value--warn {
  color: var(--td-warning-color);
}

.fxb-section {
  margin-top: 16px;
}

.fxb-section__title {
  font-weight: 600;
  margin-bottom: 8px;
}

.fxb-body {
  overflow-x: auto;
}

.fxb-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
}

.fxb-table th,
.fxb-table td {
  border: 1px solid var(--td-component-border);
  padding: 6px 10px;
  line-height: 1.6;
  white-space: nowrap;
}

.fxb-table thead th {
  background: var(--td-bg-color-secondarycontainer);
  font-weight: 600;
  text-align: center;
}

.fxb-table__name {
  text-align: left;
}

.fxb-table__num {
  text-align: right;
  font-variant-numeric: tabular-nums;
}

.fxb-table__num--total {
  font-weight: 600;
}

.fxb-table__muted {
  color: var(--td-text-color-placeholder);
  font-weight: 400;
}

.fxb-row--total td {
  background: var(--td-bg-color-container-select, #f3f6fb);
  font-weight: 600;
}

.fxb-cell--warn {
  color: var(--td-warning-color);
}

.fxb-cell--basis-warn {
  color: var(--td-warning-color);
  border-bottom: 1px dashed var(--td-warning-color);
  cursor: help;
}

.fxb-empty {
  text-align: center;
  color: var(--td-text-color-placeholder);
  padding: 24px 0;
}

.fxb-warnings {
  margin-top: 16px;
  padding: 10px 14px;
  border-radius: 6px;
  background: var(--td-warning-color-light, #fff8e6);
  color: var(--td-text-color-primary);
  font-size: 12px;
  line-height: 1.8;
}

.fxb-warnings__title {
  font-weight: 600;
  margin-bottom: 4px;
}

.fxb-warnings ul {
  margin: 0;
  padding-left: 18px;
}

.fxb-hints {
  margin: 12px 0 0;
  padding-left: 18px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 1.8;
}

.fxb-tip {
  color: var(--td-text-color-placeholder);
  text-align: center;
  padding: 40px 0;
}
</style>

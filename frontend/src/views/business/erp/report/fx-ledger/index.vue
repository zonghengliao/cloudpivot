<template>
  <div class="fxl-page">
    <t-card :bordered="false" class="fxl-card">
      <!-- ==================== 工具栏：币种 + 区间 + 科目 + 查询 ==================== -->
      <div class="tool-row">
        <span class="tool-row__label">币种</span>
        <t-select
          v-model="currency"
          class="tool-row__currency"
          :options="currencyOptions"
          filterable
          placeholder="必选（外币）"
          data-testid="fxl-currency-select"
          @change="handleFilterChange"
        />
        <span class="tool-row__label">起始</span>
        <t-date-picker
          v-model="periodFrom"
          class="tool-row__date"
          format="YYYY-MM-DD"
          clearable
          placeholder="不限"
          data-testid="fxl-period-from"
          @change="handleFilterChange"
        />
        <span class="tool-row__label">结束</span>
        <t-date-picker
          v-model="periodTo"
          class="tool-row__date"
          format="YYYY-MM-DD"
          clearable
          placeholder="不限"
          data-testid="fxl-period-to"
          @change="handleFilterChange"
        />
        <span class="tool-row__label">科目</span>
        <t-select
          v-model="accountCode"
          class="tool-row__account"
          :options="accountOptions"
          clearable
          filterable
          placeholder="全部外币科目"
          data-testid="fxl-account-select"
          @change="handleFilterChange"
        />
        <t-button theme="primary" :loading="loading" data-testid="fxl-search" @click="reload()">
          查询
        </t-button>
      </div>

      <t-alert v-if="error" theme="error" :message="error" class="fxl-alert" data-testid="fxl-error">
        <template #operation>
          <t-button size="small" variant="outline" @click="reload()">重试</t-button>
        </template>
      </t-alert>

      <!-- ==================== 表头 ==================== -->
      <div v-if="report" class="fxl-head" data-testid="fxl-head">
        <h2 class="fxl-head__title">{{ report.report_name }}</h2>
        <div class="fxl-head__meta">
          <span data-testid="fxl-company">编制单位：{{ report.company_name }}</span>
          <span data-testid="fxl-currency-text">
            币种：{{ currencyLabel(report.currency, report.currency_name) }}
          </span>
          <span data-testid="fxl-range">区间：{{ rangeText(report) }}</span>
          <span data-testid="fxl-base-currency">本位币：{{ report.base_currency }}</span>
          <span data-testid="fxl-account-text">{{ accountFilterText }}</span>
          <span data-testid="fxl-generated-at">编制时间：{{ formatDateTime(report.generated_at) }}</span>
        </div>
      </div>

      <!-- ==================== 关键指标 ==================== -->
      <div v-if="report" class="fxl-kpi" data-testid="fxl-kpi">
        <div class="fxl-kpi__item">
          <span class="fxl-kpi__label">期初原币合计</span>
          <b class="fxl-kpi__value" data-testid="fxl-opening">{{ balanceText(report.opening_fc) }}</b>
        </div>
        <div class="fxl-kpi__item">
          <span class="fxl-kpi__label">本期借（原币）</span>
          <b class="fxl-kpi__value" data-testid="fxl-period-debit">{{ formatAmount(report.period_fc_debit) }}</b>
        </div>
        <div class="fxl-kpi__item">
          <span class="fxl-kpi__label">本期贷（原币）</span>
          <b class="fxl-kpi__value" data-testid="fxl-period-credit">{{ formatAmount(report.period_fc_credit) }}</b>
        </div>
        <div class="fxl-kpi__item">
          <span class="fxl-kpi__label">期末原币净头寸</span>
          <b class="fxl-kpi__value" data-testid="fxl-closing">{{ closingTotalText(report) }}</b>
        </div>
        <div class="fxl-kpi__item">
          <span class="fxl-kpi__label">分录条数 / 科目数</span>
          <b class="fxl-kpi__value" data-testid="fxl-counts">
            {{ report.entry_count }} 条 / {{ report.accounts.length }} 科目
          </b>
        </div>
      </div>

      <!-- ==================== 按科目汇总 ==================== -->
      <div v-if="report && report.accounts.length" class="fxl-section" data-testid="fxl-summary">
        <div class="fxl-section__title">
          按科目汇总<template v-if="report.account">（只含科目 {{ report.account.account_code }}）</template>
        </div>
        <div class="fxl-body">
          <table class="fxl-table">
            <thead>
              <tr>
                <th class="fxl-table__name">科目</th>
                <th class="fxl-table__num">条数</th>
                <th class="fxl-table__num">期初原币</th>
                <th class="fxl-table__num">本期借</th>
                <th class="fxl-table__num">本期贷</th>
                <th class="fxl-table__num">期末原币</th>
                <th class="fxl-table__num">本位币借</th>
                <th class="fxl-table__num">本位币贷</th>
                <th class="fxl-table__num">本位币净额</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="(row, index) in report.accounts"
                :key="row.account_id"
                class="fxl-row"
                :data-testid="`fxl-summary-row-${index}`"
              >
                <td class="fxl-table__name">{{ row.account_code }} {{ row.account_name }}</td>
                <td class="fxl-table__num">{{ row.entry_count }}</td>
                <td class="fxl-table__num">{{ balanceText(row.opening_fc) }}</td>
                <td class="fxl-table__num">{{ formatAmount(row.period_fc_debit) }}</td>
                <td class="fxl-table__num">{{ formatAmount(row.period_fc_credit) }}</td>
                <td class="fxl-table__num fxl-table__num--total">{{ balanceText(row.closing_fc) }}</td>
                <td class="fxl-table__num">{{ formatAmount(row.period_book_debit) }}</td>
                <td class="fxl-table__num">{{ formatAmount(row.period_book_credit) }}</td>
                <td class="fxl-table__num">{{ formatAmount(row.period_book_net) }}</td>
              </tr>
              <tr class="fxl-row fxl-row--total" data-testid="fxl-summary-total">
                <td class="fxl-table__name">合计（{{ report.accounts.length }} 科目）</td>
                <td class="fxl-table__num">{{ report.entry_count }}</td>
                <td class="fxl-table__num">{{ balanceText(report.opening_fc) }}</td>
                <td class="fxl-table__num">{{ formatAmount(report.period_fc_debit) }}</td>
                <td class="fxl-table__num">{{ formatAmount(report.period_fc_credit) }}</td>
                <td class="fxl-table__num fxl-table__num--total">{{ balanceText(report.closing_fc) }}</td>
                <td class="fxl-table__num">{{ formatAmount(report.period_book_debit) }}</td>
                <td class="fxl-table__num">{{ formatAmount(report.period_book_credit) }}</td>
                <td class="fxl-table__num">{{ formatAmount(report.period_book_net) }}</td>
              </tr>
            </tbody>
          </table>
        </div>
        <div class="fxl-note" data-testid="fxl-book-note">
          本位币三列：{{ bookColumnHint(report.currency, report.base_currency) }}
        </div>
      </div>

      <!-- ==================== 明细流水 ==================== -->
      <div v-if="report" class="fxl-section" data-testid="fxl-table">
        <div class="fxl-section__title">明细流水（共 {{ report.total }} 行）</div>
        <t-loading :loading="loading" size="small">
          <div class="fxl-body">
            <table class="fxl-table">
              <thead>
                <tr>
                  <th class="fxl-table__name">日期</th>
                  <th class="fxl-table__name">凭证字号</th>
                  <th class="fxl-table__num">行</th>
                  <th class="fxl-table__name fxl-table__summary">摘要</th>
                  <th class="fxl-table__name">科目</th>
                  <th class="fxl-table__num">汇率</th>
                  <th class="fxl-table__num">原币借</th>
                  <th class="fxl-table__num">原币贷</th>
                  <th class="fxl-table__num">本位币借</th>
                  <th class="fxl-table__num">本位币贷</th>
                  <th class="fxl-table__num">原币余额</th>
                  <th class="fxl-table__name">标记</th>
                </tr>
              </thead>
              <tbody>
                <tr
                  v-for="(row, index) in report.rows"
                  :key="`${row.voucher_id}-${row.line_no}`"
                  class="fxl-row"
                  :data-testid="`fxl-row-${index}`"
                >
                  <td class="fxl-table__name">{{ row.voucher_date }}</td>
                  <td class="fxl-table__name">{{ row.voucher_word }}-{{ row.voucher_no }}</td>
                  <td class="fxl-table__num">{{ row.line_no }}</td>
                  <td class="fxl-table__name fxl-table__summary" :title="row.summary">{{ row.summary }}</td>
                  <td class="fxl-table__name" :data-testid="`fxl-row-account-${index}`">
                    {{ row.account_code }} {{ row.account_name }}
                  </td>
                  <td class="fxl-table__num">{{ rateText(row.exchange_rate) }}</td>
                  <td class="fxl-table__num">{{ formatAmount(row.fc_debit) }}</td>
                  <td class="fxl-table__num">{{ formatAmount(row.fc_credit) }}</td>
                  <td class="fxl-table__num">{{ formatAmount(row.debit_amount) }}</td>
                  <td class="fxl-table__num">{{ formatAmount(row.credit_amount) }}</td>
                  <td
                    class="fxl-table__num fxl-table__num--total"
                    :data-testid="`fxl-row-running-${index}`"
                  >
                    {{ balanceText(row.running_fc_balance) }}
                  </td>
                  <td class="fxl-table__name">
                    <span v-if="row.is_red" class="fxl-red">{{ RED_LABEL }}</span>
                    <span v-else class="fxl-table__muted">—</span>
                  </td>
                </tr>
                <tr v-if="!report.rows.length">
                  <td class="fxl-empty" colspan="12" data-testid="fxl-empty">{{ emptyText(report) }}</td>
                </tr>
              </tbody>
            </table>
          </div>
        </t-loading>
        <div class="fxl-note" data-testid="fxl-running-note">{{ runningColumnHint() }}</div>
        <div v-if="report.total > 0" class="pager-row">
          <t-pagination
            v-model="page"
            v-model:page-size="pageSize"
            :total="report.total"
            :page-size-options="[20, 50, 100, 200]"
            data-testid="fxl-pager"
            @change="loadPage"
          />
        </div>
      </div>

      <!-- ==================== 数据质量问题 ==================== -->
      <div v-if="report && report.warnings.length" class="fxl-warnings" data-testid="fxl-warnings">
        <div class="fxl-warnings__title">需要人工处理</div>
        <ul>
          <li v-for="(warning, index) in report.warnings" :key="index">{{ warning }}</li>
        </ul>
      </div>

      <!-- ==================== 口径提示 ==================== -->
      <ul v-if="report && report.hints.length" class="fxl-hints" data-testid="fxl-hints">
        <li v-for="(hint, index) in report.hints" :key="index">{{ hint }}</li>
      </ul>

      <div v-if="!report && !loading && !error" class="fxl-tip" data-testid="fxl-tip">
        选择币种（可再选区间与科目）后点「查询」，查外币明细账
      </div>
    </t-card>
  </div>
</template>

<script setup lang="ts">
/**
 * 外币明细账（M1-08 · 迁移 `V0413`）。
 *
 * # 唯一口径：后端 `/business/erp/report/fx-ledger`
 *
 * 页面**不算账**：期初 / 本期 / 期末、逐行余额、按科目汇总全部由后端算好
 * （取数口径与外币余额表 M1-07 逐字一致，两张表可按科目对拍）。前端只做三件事：
 * 1. 把十进制**字符串**金额格式化成千分位（`fx-ledger-logic.ts`，不过 `Number()`）；
 * 2. 有符号余额按「借 / 贷 / 平」显示，**不取绝对值**（资产负债共用一套公式）；
 * 3. 把后端给的 `warnings` / `hints` 原样呈现（口径说明不写在前端，避免两处漂移）。
 *
 * # 为什么币种是必选、且下拉里没有本位币
 *
 * 本位币没有原币口径（`fc_*` 为 NULL），查本位币流水应当用「总账 / 科目余额表」。
 * 后端对 `currency` = 本位币会返回 400 并指路，前端就不给这个选项（少一个坑）。
 */
import { computed, onMounted, ref } from 'vue';

import { fetchAccountOptions, type AccountRow } from '@/api/business/erp';
import { fetchCurrencyOptions, type CurrencyOption } from '@/api/business/erp/currency';
import { fetchFxLedger, type FxLedgerReport } from '@/api/business/erp/fx-ledger';
import {
  RED_LABEL,
  balanceText,
  bookColumnHint,
  closingTotalText,
  currencyLabel,
  emptyText,
  formatAmount,
  rangeText,
  rateText,
  runningColumnHint,
} from './fx-ledger-logic';

const currency = ref('');
const currencyOptions = ref<{ label: string; value: string }[]>([]);
const periodFrom = ref('');
const periodTo = ref('');
const accountCode = ref('');
const accountOptions = ref<{ label: string; value: string }[]>([]);
const loading = ref(false);
const error = ref('');
const report = ref<FxLedgerReport | null>(null);
const page = ref(1);
const pageSize = ref(20);

const accountFilterText = computed(() => {
  const echo = report.value?.account;
  return echo ? `科目：${echo.account_code} ${echo.account_name}` : '科目：全部外币核算科目';
});

/** 时间戳格式化（与其它报表页同一份实现，页面只读） */
function formatDateTime(value: string): string {
  if (!value) return '';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

/** 币种下拉：只列启用的**外币**（本位币没有原币口径，后端会 400） */
async function loadCurrencies() {
  try {
    const list = await fetchCurrencyOptions();
    currencyOptions.value = (list as CurrencyOption[])
      .filter((item) => !item.is_base)
      .map((item) => ({ label: item.label, value: item.code }));
    if (!currency.value && currencyOptions.value.length) {
      currency.value = currencyOptions.value[0].value;
    }
  } catch {
    // 币种列表拉不到只影响筛选体验，不挡查询（可手填）
  }
}

/** 科目下拉：只列已标记「外币核算」的末级科目（其它科目查出来必然是空表） */
async function loadAccounts() {
  try {
    const list = await fetchAccountOptions();
    accountOptions.value = (list as AccountRow[])
      .filter((row) => row.is_foreign_currency && row.is_leaf)
      .map((row) => ({ label: `${row.account_code} ${row.account_name}`, value: row.account_code }));
  } catch {
    // 同上：科目列表拉不到只是少一个筛选项
  }
}

/** 查一页（`page` 由分页器维护；筛选变化时先归零再查） */
async function loadPage() {
  if (!currency.value) {
    error.value = '请先选择币种';
    return;
  }
  loading.value = true;
  error.value = '';
  try {
    report.value = await fetchFxLedger({
      currency: currency.value,
      period_from: periodFrom.value || undefined,
      period_to: periodTo.value || undefined,
      account_code: accountCode.value || undefined,
      page: page.value,
      page_size: pageSize.value,
    });
  } catch (err) {
    report.value = null;
    error.value = (err as Error).message ?? String(err);
  } finally {
    loading.value = false;
  }
}

/** 筛选条件变化 → 回到第 1 页再查（否则会停在不存在的页码上） */
async function reload() {
  page.value = 1;
  await loadPage();
}

function handleFilterChange() {
  void reload();
}

onMounted(async () => {
  await Promise.all([loadCurrencies(), loadAccounts()]);
  await reload();
});
</script>

<style scoped>
.fxl-card {
  min-height: 200px;
}

.fxl-alert {
  margin-bottom: 12px;
}

.fxl-head {
  text-align: center;
  margin: 8px 0 12px;
}

.fxl-head__title {
  margin: 0 0 6px;
  font-size: 20px;
  font-weight: 600;
  letter-spacing: 2px;
}

.fxl-head__meta {
  display: flex;
  justify-content: center;
  gap: 20px;
  flex-wrap: wrap;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.fxl-kpi {
  display: flex;
  gap: 12px;
  flex-wrap: wrap;
  margin-bottom: 12px;
}

.fxl-kpi__item {
  flex: 1 1 180px;
  padding: 10px 14px;
  border-radius: 6px;
  background: var(--td-bg-color-secondarycontainer);
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.fxl-kpi__label {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.fxl-kpi__value {
  font-size: 18px;
  font-weight: 600;
  font-variant-numeric: tabular-nums;
}

.fxl-section {
  margin-top: 16px;
}

.fxl-section__title {
  font-weight: 600;
  margin-bottom: 8px;
}

.fxl-body {
  overflow-x: auto;
}

.fxl-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
}

.fxl-table th,
.fxl-table td {
  border: 1px solid var(--td-component-border);
  padding: 6px 10px;
  line-height: 1.6;
  white-space: nowrap;
}

.fxl-table thead th {
  background: var(--td-bg-color-secondarycontainer);
  font-weight: 600;
  text-align: center;
}

.fxl-table__name {
  text-align: left;
}

.fxl-table__summary {
  max-width: 260px;
  overflow: hidden;
  text-overflow: ellipsis;
}

.fxl-table__num {
  text-align: right;
  font-variant-numeric: tabular-nums;
}

.fxl-table__num--total {
  font-weight: 600;
}

.fxl-table__muted {
  color: var(--td-text-color-placeholder);
}

.fxl-row--total td {
  background: var(--td-bg-color-container-select, #f3f6fb);
  font-weight: 600;
}

.fxl-red {
  color: var(--td-error-color);
  font-weight: 600;
}

.fxl-empty {
  text-align: center;
  color: var(--td-text-color-placeholder);
  padding: 24px 0;
}

.fxl-note {
  margin-top: 6px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.pager-row {
  display: flex;
  justify-content: flex-end;
  margin-top: 12px;
}

.fxl-warnings {
  margin-top: 16px;
  padding: 10px 14px;
  border-radius: 6px;
  background: var(--td-warning-color-light, #fff8e6);
  color: var(--td-text-color-primary);
  font-size: 12px;
  line-height: 1.8;
}

.fxl-warnings__title {
  font-weight: 600;
  margin-bottom: 4px;
}

.fxl-warnings ul {
  margin: 0;
  padding-left: 18px;
}

.fxl-hints {
  margin: 12px 0 0;
  padding-left: 18px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 1.8;
}

.fxl-tip {
  color: var(--td-text-color-placeholder);
  text-align: center;
  padding: 40px 0;
}
</style>

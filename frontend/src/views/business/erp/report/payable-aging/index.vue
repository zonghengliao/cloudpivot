<template>
  <div class="pa-page">
    <t-card :bordered="false" class="pa-card">
      <!-- ==================== 工具栏：期间 + 基准日 + 查询 + 导出 ==================== -->
      <div class="tool-row">
        <span class="tool-row__label">会计期间</span>
        <t-select
          v-model="period"
          class="tool-row__period"
          :options="periodOptions"
          :loading="periodLoading"
          clearable
          placeholder="不选则按基准日"
          data-testid="pa-period-select"
          @change="handlePeriodChange"
        />
        <span class="tool-row__label">基准日</span>
        <t-date-picker
          v-model="asOf"
          class="tool-row__date"
          value-type="YYYY-MM-DD"
          allow-input
          clearable
          placeholder="留空 = 该期间末日"
          data-testid="pa-asof"
          @change="handleAsOfChange"
        />
        <t-button theme="primary" :loading="loading" data-testid="pa-search" @click="load()">查询</t-button>
        <t-button variant="outline" :disabled="!report" data-testid="pa-export" @click="handleExport">
          <template #icon><t-icon name="download" /></template>
          导出 Excel
        </t-button>
      </div>

      <t-alert v-if="error" theme="error" :message="error" class="pa-alert" data-testid="pa-error">
        <template #operation>
          <t-button size="small" variant="outline" @click="load()">重试</t-button>
        </template>
      </t-alert>

      <!-- ==================== 表头 ==================== -->
      <div v-if="report" class="pa-head" data-testid="pa-head">
        <h2 class="pa-head__title">{{ report.report_name }}</h2>
        <div class="pa-head__meta">
          <span data-testid="pa-company">编制单位：{{ report.company_name }}</span>
          <span data-testid="pa-asof-text">基准日：{{ report.as_of }}（{{ asOfSourceText(report.as_of_source) }}）</span>
          <span v-if="report.period" data-testid="pa-period-text">
            期间：{{ report.period_name }}<template v-if="report.period_status">（{{ periodStatusText(report.period_status) }}）</template>
          </span>
          <span>单位：{{ report.unit }}</span>
          <span data-testid="pa-generated-at">编制时间：{{ formatDateTime(report.generated_at) }}</span>
        </div>
      </div>

      <!-- ==================== 关键指标 ==================== -->
      <div v-if="report" class="pa-kpi" data-testid="pa-kpi">
        <div class="pa-kpi__item">
          <span class="pa-kpi__label">未付合计</span>
          <b class="pa-kpi__value" data-testid="pa-total-unpaid">{{ formatAmount(report.total_unpaid) }}</b>
        </div>
        <div class="pa-kpi__item">
          <span class="pa-kpi__label">逾期合计</span>
          <b class="pa-kpi__value pa-kpi__value--warn" data-testid="pa-overdue-unpaid">{{ formatAmount(report.overdue_unpaid) }}</b>
        </div>
        <div class="pa-kpi__item">
          <span class="pa-kpi__label">逾期占比</span>
          <b class="pa-kpi__value" data-testid="pa-overdue-ratio">{{ report.overdue_ratio }}%</b>
        </div>
        <div class="pa-kpi__item">
          <span class="pa-kpi__label">供应商 / 单据</span>
          <b class="pa-kpi__value" data-testid="pa-counts">{{ report.supplier_count }} 家 / {{ report.doc_count }} 张</b>
        </div>
      </div>

      <!-- ==================== 供应商账龄汇总 ==================== -->
      <t-loading :loading="loading" size="small">
        <div v-if="report" class="pa-body" data-testid="pa-table">
          <table class="pa-table">
            <thead>
              <tr>
                <th class="pa-table__name">供应商</th>
                <th class="pa-table__num">单据数</th>
                <th v-for="bucket in report.buckets" :key="bucket.key" class="pa-table__num">
                  {{ bucket.label }}
                </th>
                <th class="pa-table__num pa-table__num--total">未付合计</th>
                <th class="pa-table__num">最长逾期(天)</th>
              </tr>
            </thead>
            <tbody>
              <tr class="pa-row pa-row--total" data-testid="pa-row-total">
                <td class="pa-table__name">合计（{{ report.supplier_count }} 家）</td>
                <td class="pa-table__num">{{ report.doc_count }}</td>
                <td
                  v-for="cell in report.summary"
                  :key="cell.key"
                  class="pa-table__num"
                  :data-testid="`pa-sum-${cell.key}`"
                >
                  {{ formatAmount(cell.unpaid) }}
                </td>
                <td class="pa-table__num pa-table__num--total" data-testid="pa-sum-total">
                  {{ formatAmount(report.total_unpaid) }}
                </td>
                <td class="pa-table__num">{{ maxOverdueDays }}</td>
              </tr>
              <tr
                v-for="(supplier, index) in report.suppliers"
                :key="supplier.supplier_id"
                class="pa-row"
                :data-testid="`pa-supplier-${index}`"
              >
                <td class="pa-table__name" :data-testid="`pa-supplier-name-${index}`">
                  {{ supplier.supplier_name || '（未命名供应商）' }}
                </td>
                <td class="pa-table__num">{{ supplier.doc_count }}</td>
                <td
                  v-for="cell in supplier.buckets"
                  :key="cell.key"
                  class="pa-table__num"
                  :class="{ 'pa-cell--zero': isZero(cell.unpaid) }"
                >
                  {{ formatAmount(cell.unpaid) }}
                </td>
                <td class="pa-table__num pa-table__num--total">{{ formatAmount(supplier.total_unpaid) }}</td>
                <td class="pa-table__num" :class="{ 'pa-cell--overdue': supplier.max_overdue_days > 0 }">
                  {{ supplier.max_overdue_days }}
                </td>
              </tr>
              <tr v-if="!report.suppliers.length">
                <td class="pa-empty" :colspan="report.buckets.length + 4" data-testid="pa-empty">
                  基准日 {{ report.as_of }} 没有未付清的应付单（这不是出错）
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </t-loading>

      <!-- ==================== 单据明细 ==================== -->
      <div v-if="report && report.details.length" class="pa-detail" data-testid="pa-detail">
        <div class="pa-detail__title">
          单据明细（共 {{ report.detail_count }} 张<template v-if="report.detail_truncated">，本页仅列前 {{ report.details.length }} 张</template>）
        </div>
        <table class="pa-table pa-table--detail">
          <thead>
            <tr>
              <th class="pa-table__name">应付单号</th>
              <th class="pa-table__name">供应商</th>
              <th class="pa-table__name">来源单号</th>
              <th class="pa-table__num">所属期间</th>
              <th class="pa-table__num">到期日</th>
              <th class="pa-table__num">账期</th>
              <th class="pa-table__num">逾期天数</th>
              <th class="pa-table__name">账龄档</th>
              <th class="pa-table__num">应付</th>
              <th class="pa-table__num">已付</th>
              <th class="pa-table__num pa-table__num--total">未付</th>
              <th class="pa-table__name">状态</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="(row, index) in report.details"
              :key="row.payable_id"
              class="pa-row"
              :data-testid="`pa-detail-row-${index}`"
            >
              <td class="pa-table__name">{{ row.payable_no }}</td>
              <td class="pa-table__name">{{ row.supplier_name || '（未命名供应商）' }}</td>
              <td class="pa-table__name">{{ row.source_no }}</td>
              <td class="pa-table__num">{{ row.period_code }}</td>
              <td class="pa-table__num">{{ row.due_date }}</td>
              <td class="pa-table__num">{{ row.credit_days }} 天</td>
              <td class="pa-table__num" :class="{ 'pa-cell--overdue': row.overdue_days > 0 }" :data-testid="`pa-detail-days-${index}`">
                {{ overdueText(row.overdue_days) }}
              </td>
              <td class="pa-table__name" :data-testid="`pa-detail-bucket-${index}`">{{ row.bucket_label }}</td>
              <td class="pa-table__num">{{ formatAmount(row.amount) }}</td>
              <td class="pa-table__num">{{ formatAmount(row.paid_amount) }}</td>
              <td class="pa-table__num pa-table__num--total">{{ formatAmount(row.unpaid_amount) }}</td>
              <td class="pa-table__name">{{ statusText(row.status) }}</td>
            </tr>
          </tbody>
        </table>
      </div>

      <!-- ==================== 口径提示 ==================== -->
      <ul v-if="report && report.hints.length" class="pa-hints" data-testid="pa-hints">
        <li v-for="(hint, index) in report.hints" :key="index">{{ hint }}</li>
      </ul>

      <div v-if="!report && !loading && !error" class="pa-tip" data-testid="pa-tip">
        选择期间或基准日后点「查询」，编应付账款账龄分析表
      </div>
    </t-card>
  </div>
</template>

<script setup lang="ts">
/**
 * 应付账款账龄分析（P1-FIX-8A / PRD E4-03 P0）。
 *
 * # 唯一口径：后端 `/business/erp/report/payable-aging`
 *
 * 页面**不算账**：分档、汇总、逾期占比、最长逾期天数全部由后端算好。前端只做两件事：
 * 1. 按 `report.buckets` 渲染表头（档位由后端定义，加档位前端自动跟上，不写死）；
 * 2. 把字符串金额格式化成千分位（`Number()` 会引入浮点误差，报表金额不能这么玩）。
 *
 * # 基准日怎么来的
 *
 * 选期间 → 基准日自动填该期间末日（`as_of_source = PERIOD`）；手改基准日 → `QUERY`；
 * 两个都空 → 后端按今天（CST）算，`TODAY`。后端在 `as_of_source` 里回显，页面照实写。
 */
import { computed, onMounted, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';

import {
  exportPayableAging,
  fetchPayableAging,
  fetchPeriodPage,
  type AccountPeriodRow,
  type PayableAging as PayableAgingData,
  type ReportPeriodStatus,
} from '@/api/business/erp';
import { saveBlob } from '@/utils/download';

const period = ref('');
const periodOptions = ref<{ label: string; value: string }[]>([]);
const periodLoading = ref(false);
const asOf = ref('');
const loading = ref(false);
const error = ref('');
const report = ref<PayableAgingData | null>(null);

/** 全表最长逾期天数（合计行的「最长逾期」列） */
const maxOverdueDays = computed(() =>
  report.value ? report.value.suppliers.reduce((max, item) => Math.max(max, item.max_overdue_days), 0) : 0,
);

/**
 * 千分位 + 2 位小数（与资产负债表页同一份实现）。
 *
 * **不用 `Number()` 做四舍五入**：`Number('0.595').toFixed(2)` 会得到 `0.59`（二进制浮点误差），
 * 报表上少一分钱是事故。这里按十进制字符串四舍五入到分。
 */
function formatAmount(value: string | null | undefined): string {
  if (value === null || value === undefined || value === '') return '';
  const text = String(value).trim();
  const negative = text.startsWith('-');
  const clean = negative ? text.slice(1) : text;
  const [rawInt, rawFrac = ''] = clean.split('.');
  const intPart = rawInt || '0';
  const frac4 = `${rawFrac}0000`.slice(0, 4);

  let digits = (intPart + frac4.slice(0, 2)).split('').map(Number);
  if (frac4[2] >= '5') {
    let index = digits.length - 1;
    while (index >= 0) {
      digits[index] += 1;
      if (digits[index] === 10) {
        digits[index] = 0;
        index -= 1;
      } else {
        break;
      }
    }
    if (index < 0) digits = [1, ...digits];
  }
  const joined = digits.join('') || '0';
  const cents = joined.slice(-2).padStart(2, '0');
  const yuan = (joined.length > 2 ? joined.slice(0, -2) : '0').replace(/^0+(?=\d)/, '');
  const grouped = yuan.replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  const isZero = /^0+$/.test(joined);
  return `${negative && !isZero ? '-' : ''}${grouped}.${cents}`;
}

function isZero(value: string): boolean {
  return Number(value) === 0;
}

/** 逾期天数的中文口径：0 = 到期当天，负数 = 未到期（还在账期内）。 */
function overdueText(days: number): string {
  if (days > 0) return `逾期 ${days}`;
  if (days === 0) return '到期当天';
  return `未到期 ${-days}`;
}

function statusText(status: string): string {
  switch (status) {
    case 'OPEN':
      return '未付';
    case 'PARTIAL_PAID':
      return '部分已付';
    case 'PENDING_CANCEL':
      return '作废审批中';
    default:
      return status;
  }
}

function asOfSourceText(source: PayableAgingData['as_of_source']): string {
  switch (source) {
    case 'QUERY':
      return '手动指定';
    case 'PERIOD':
      return '期间末日';
    default:
      return '今天';
  }
}

function periodStatusText(status: ReportPeriodStatus): string {
  switch (status) {
    case 'CLOSED':
      return '已结账';
    case 'DERIVED':
      return '未开账·按自然月推算';
    default:
      return '未结账';
  }
}

function formatDateTime(value: string): string {
  if (!value) return '';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

/** 会计期间下拉：只列「已建立」的期间（NOT_OPENED 的没意义），按期间倒序 */
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
      // 基准日留空 = 由后端按「该期间末日」算，页头会写「期间末日」；
      // 前端自己填一个末日再传上去，后端只能认为这是"手填"，来源就永远显示「手动指定」了。
      asOf.value = '';
    }
  } catch (err) {
    // 期间列表拉不到不影响出表：可以只按基准日查
    error.value = `期间列表加载失败：${(err as Error).message ?? err}`;
  } finally {
    periodLoading.value = false;
  }
}

async function load() {
  loading.value = true;
  error.value = '';
  try {
    report.value = await fetchPayableAging({
      period: period.value || undefined,
      as_of: asOf.value || undefined,
    });
  } catch (err) {
    report.value = null;
    error.value = (err as Error).message ?? String(err);
  } finally {
    loading.value = false;
  }
}

/** 切期间 → 基准日清空（= 用该期间末日），然后重算 */
function handlePeriodChange() {
  asOf.value = '';
  void load();
}

/** 只改基准日不重算，等用户点「查询」（避免日期选择器逐字符搜索） */
function handleAsOfChange() {
  // 故意留空：统一由「查询」按钮触发
}

async function handleExport() {
  if (!report.value) return;
  try {
    const blob = await exportPayableAging({
      period: period.value || undefined,
      as_of: asOf.value || undefined,
    });
    saveBlob(blob, `应付账款账龄分析表_截止${report.value.as_of}.xlsx`);
    MessagePlugin.success('已导出');
  } catch (err) {
    MessagePlugin.error(`导出失败：${(err as Error).message ?? err}`);
  }
}

onMounted(async () => {
  await loadPeriods();
  await load();
});
</script>

<style scoped>
.pa-card {
  min-height: 200px;
}

.tool-row {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
  margin-bottom: 12px;
}

.tool-row__label {
  color: var(--td-text-color-secondary);
}

.tool-row__period {
  width: 200px;
}

.tool-row__date {
  width: 180px;
}

.pa-alert {
  margin-bottom: 12px;
}

.pa-head {
  text-align: center;
  margin: 8px 0 12px;
}

.pa-head__title {
  margin: 0 0 6px;
  font-size: 20px;
  font-weight: 600;
  letter-spacing: 2px;
}

.pa-head__meta {
  display: flex;
  justify-content: center;
  gap: 20px;
  flex-wrap: wrap;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.pa-kpi {
  display: flex;
  gap: 12px;
  flex-wrap: wrap;
  margin-bottom: 12px;
}

.pa-kpi__item {
  flex: 1 1 180px;
  padding: 10px 14px;
  border-radius: 6px;
  background: var(--td-bg-color-secondarycontainer);
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.pa-kpi__label {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.pa-kpi__value {
  font-size: 18px;
  font-weight: 600;
}

.pa-kpi__value--warn {
  color: var(--td-error-color);
}

.pa-body {
  overflow-x: auto;
}

.pa-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
}

.pa-table th,
.pa-table td {
  border: 1px solid var(--td-component-border);
  padding: 6px 10px;
  line-height: 1.6;
}

.pa-table thead th {
  background: var(--td-bg-color-secondarycontainer);
  font-weight: 600;
  text-align: center;
}

.pa-table__name {
  text-align: left;
}

.pa-table__num {
  text-align: right;
  font-variant-numeric: tabular-nums;
}

.pa-table__num--total {
  font-weight: 600;
}

.pa-row--total td {
  background: var(--td-bg-color-container-select, #f3f6fb);
  font-weight: 600;
}

.pa-cell--zero {
  color: var(--td-text-color-placeholder);
}

.pa-cell--overdue {
  color: var(--td-error-color);
  font-weight: 600;
}

.pa-empty {
  text-align: center;
  color: var(--td-text-color-placeholder);
  padding: 24px 0;
}

.pa-detail {
  margin-top: 20px;
}

.pa-detail__title {
  font-weight: 600;
  margin-bottom: 8px;
}

.pa-table--detail {
  font-size: 12px;
}

.pa-hints {
  margin: 12px 0 0;
  padding-left: 18px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 1.8;
}

.pa-tip {
  color: var(--td-text-color-placeholder);
  text-align: center;
  padding: 40px 0;
}
</style>

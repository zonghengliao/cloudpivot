<template>
  <div class="ra-page">
    <t-card :bordered="false" class="ra-card">
      <!-- ==================== 工具栏：期间 + 基准日 + 客户筛选 + 排序 + 查询/导出 ==================== -->
      <div class="tool-row ra-tool">
        <span class="tool-row__label ra-tool__label">会计期间</span>
        <t-select
          v-model="period"
          class="ra-tool__period"
          :options="periodOptions"
          :loading="periodLoading"
          filterable
          clearable
          placeholder="不选则按基准日"
          data-testid="ra-period-select"
          @change="handlePeriodChange"
        />
        <span class="tool-row__label ra-tool__label">基准日</span>
        <t-date-picker
          v-model="asOf"
          class="ra-tool__date"
          value-type="YYYY-MM-DD"
          allow-input
          clearable
          placeholder="留空 = 该期间末日 / 今天"
          data-testid="ra-asof"
          @change="handleAsOfChange"
        />
        <t-button theme="primary" :loading="loading" data-testid="ra-search" @click="load()">查询</t-button>
        <t-button
          variant="outline"
          :disabled="!report"
          :loading="exporting"
          data-testid="ra-export"
          @click="handleExport"
        >
          <template #icon><t-icon name="download" /></template>
          导出 Excel
        </t-button>
      </div>

      <div class="tool-row ra-tool ra-tool--sub">
        <span class="tool-row__label ra-tool__label">客户</span>
        <t-input
          v-model="keyword"
          class="ra-tool__search"
          clearable
          placeholder="按客户名筛选（不改合计）"
          data-testid="ra-keyword"
        />
        <span class="tool-row__label ra-tool__label">排序</span>
        <t-select
          v-model="sortMode"
          class="ra-tool__sort"
          :options="CUSTOMER_SORT_OPTIONS"
          data-testid="ra-sort"
        />
        <span class="ra-tool__spacer" />
        <span class="ra-tool__muted">
          分档口径与应付账龄同源（后端 <span class="mono">AGING_BUCKETS</span>），前端不写死 30/60/90
        </span>
      </div>

      <t-alert v-if="error" theme="error" :message="error" class="ra-alert" data-testid="ra-error">
        <template #operation>
          <t-button size="small" variant="outline" @click="load()">重试</t-button>
        </template>
      </t-alert>

      <!-- ==================== 表头 ==================== -->
      <div v-if="report" class="ra-head" data-testid="ra-head">
        <h2 class="ra-head__title">{{ report.report_name }}</h2>
        <div class="ra-head__meta">
          <span data-testid="ra-company">编制单位：{{ report.company_name }}</span>
          <span data-testid="ra-asof-text">
            基准日：{{ report.as_of }}（{{ asOfSourceText(report.as_of_source) }}）
          </span>
          <span v-if="report.period" data-testid="ra-period-text">
            期间：{{ report.period_name || report.period
            }}<template v-if="report.period_status">（{{ periodStatusText(report.period_status) }}）</template>
          </span>
          <span>单位：{{ report.unit }}</span>
          <span data-testid="ra-generated-at">编制时间：{{ formatDateTime(report.generated_at) }}</span>
        </div>
      </div>

      <!-- ==================== 关键指标 ==================== -->
      <div v-if="report" class="ra-kpi" data-testid="ra-kpi">
        <div class="ra-kpi__item">
          <span class="ra-kpi__label">未收合计</span>
          <b class="ra-kpi__value" data-testid="ra-total-unsettled">{{ formatAmount(report.total_unsettled) }}</b>
        </div>
        <div class="ra-kpi__item">
          <span class="ra-kpi__label">逾期合计</span>
          <b class="ra-kpi__value ra-kpi__value--warn" data-testid="ra-overdue-unsettled">
            {{ formatAmount(report.overdue_unsettled) }}
          </b>
        </div>
        <div class="ra-kpi__item">
          <span class="ra-kpi__label">逾期占比</span>
          <b class="ra-kpi__value" data-testid="ra-overdue-ratio">{{ report.overdue_ratio }}%</b>
        </div>
        <div class="ra-kpi__item">
          <span class="ra-kpi__label">客户 / 单据</span>
          <b class="ra-kpi__value" data-testid="ra-counts">
            {{ report.customer_count }} 家 / {{ report.doc_count }} 张
          </b>
        </div>
        <div class="ra-kpi__item">
          <span class="ra-kpi__label">范围内应收 / 已收</span>
          <b class="ra-kpi__value" data-testid="ra-scope-totals">
            {{ formatAmount(report.total_amount) }} / {{ formatAmount(report.total_received) }}
          </b>
        </div>
      </div>

      <!-- ==================== 客户账龄汇总 ==================== -->
      <t-loading :loading="loading" size="small">
        <div v-if="report" class="ra-body" data-testid="ra-table">
          <div class="ra-section__title">
            客户汇总
            <span v-if="filterActive" class="ra-section__note" data-testid="ra-filter-note">
              已按「{{ keyword.trim() }}」筛出 {{ visibleCustomers.length }} / {{ report.customer_count }} 家；
              合计行仍是全表数（不按筛选重算）
            </span>
          </div>
          <table class="ra-table">
            <thead>
              <tr>
                <th class="ra-table__name">客户</th>
                <th class="ra-table__num">单据数</th>
                <th v-for="bucket in report.buckets" :key="bucket.key" class="ra-table__num">
                  {{ bucket.label }}
                </th>
                <th class="ra-table__num ra-table__num--total">未收合计</th>
                <th class="ra-table__num">最长逾期(天)</th>
              </tr>
            </thead>
            <tbody>
              <tr class="ra-row ra-row--total" data-testid="ra-row-total">
                <td class="ra-table__name">合计（全表 {{ report.customer_count }} 家）</td>
                <td class="ra-table__num">{{ report.doc_count }}</td>
                <td
                  v-for="cell in report.summary"
                  :key="cell.key"
                  class="ra-table__num"
                  :data-testid="`ra-sum-${cell.key}`"
                >
                  <span :class="{ 'ra-cell--zero': isZeroAmount(cell.unpaid) }">{{ formatAmount(cell.unpaid) }}</span>
                  <span class="ra-cell__bar">
                    <i
                      :style="{
                        width: `${bucketShare(cell.unpaid, report.total_unsettled)}%`,
                        background: bucketColor(cell.key),
                      }"
                    />
                  </span>
                </td>
                <td class="ra-table__num ra-table__num--total" data-testid="ra-sum-total">
                  {{ formatAmount(report.total_unsettled) }}
                </td>
                <td class="ra-table__num">{{ maxOverdueDays(report) }}</td>
              </tr>
              <tr
                v-for="(customer, index) in visibleCustomers"
                :key="customer.customer_id"
                class="ra-row ra-row--clickable"
                :class="{ 'ra-row--selected': customer.customer_id === selectedCustomerId }"
                :data-testid="`ra-customer-${index}`"
                @click="toggleCustomer(customer)"
              >
                <td class="ra-table__name" :data-testid="`ra-customer-name-${index}`">
                  {{ customer.customer_name || '（客户已删除）' }}
                </td>
                <td class="ra-table__num">{{ customer.doc_count }}</td>
                <td v-for="cell in customer.buckets" :key="cell.key" class="ra-table__num">
                  <span :class="{ 'ra-cell--zero': isZeroAmount(cell.unpaid) }">{{ formatAmount(cell.unpaid) }}</span>
                </td>
                <td class="ra-table__num ra-table__num--total">{{ formatAmount(customer.total_unsettled) }}</td>
                <td class="ra-table__num" :class="{ 'ra-cell--overdue': customer.max_overdue_days > 0 }">
                  {{ customer.max_overdue_days }}
                </td>
              </tr>
              <tr v-if="!visibleCustomers.length">
                <td class="ra-empty" :colspan="report.buckets.length + 4" data-testid="ra-empty">
                  <template v-if="report.customers.length">
                    没有客户名包含「{{ keyword.trim() }}」的客户（全表 {{ report.customer_count }} 家）
                  </template>
                  <template v-else>基准日 {{ report.as_of }} 没有未收清的应收单（这不是出错）</template>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </t-loading>

      <!-- ==================== 单据明细 ==================== -->
      <div v-if="report && report.details.length" class="ra-detail" data-testid="ra-detail">
        <div class="ra-detail__title">
          单据明细
          <span class="ra-detail__scope" data-testid="ra-detail-scope">
            {{ detailsScopeText(visibleDetails.length, report.detail_count, selectedCustomerName, report.detail_truncated) }}
          </span>
          <t-button
            v-if="selectedCustomerId"
            size="small"
            variant="text"
            data-testid="ra-detail-clear"
            @click="clearSelectedCustomer()"
          >
            看全部客户
          </t-button>
        </div>
        <div class="ra-body">
          <table class="ra-table ra-table--detail">
            <thead>
              <tr>
                <th class="ra-table__name">应收单号</th>
                <th class="ra-table__name">客户</th>
                <th class="ra-table__name">来源单号</th>
                <th class="ra-table__name">来源类型</th>
                <th class="ra-table__name">发票号</th>
                <th class="ra-table__num">所属期间</th>
                <th class="ra-table__num">到期日</th>
                <th class="ra-table__num">账期</th>
                <th class="ra-table__num">逾期天数</th>
                <th class="ra-table__name">账龄档</th>
                <th class="ra-table__num">应收</th>
                <th class="ra-table__num">已收</th>
                <th class="ra-table__num ra-table__num--total">未收</th>
                <th class="ra-table__name">状态</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="(row, index) in visibleDetails"
                :key="row.receivable_id"
                class="ra-row"
                :data-testid="`ra-detail-row-${index}`"
              >
                <td class="ra-table__name mono" :data-testid="`ra-detail-no-${index}`">{{ row.ar_no }}</td>
                <td class="ra-table__name">{{ row.customer_name || '（客户已删除）' }}</td>
                <td class="ra-table__name mono">{{ dash(row.source_no) }}</td>
                <td class="ra-table__name">{{ sourceTypeText(row.source_type) }}</td>
                <td class="ra-table__name mono">{{ dash(row.invoice_no) }}</td>
                <td class="ra-table__num mono">{{ row.period_code }}</td>
                <td class="ra-table__num">{{ row.due_date }}</td>
                <td class="ra-table__num">{{ row.credit_days }} 天</td>
                <td
                  class="ra-table__num"
                  :class="{ 'ra-cell--overdue': row.overdue_days > 0 }"
                  :data-testid="`ra-detail-days-${index}`"
                >
                  {{ overdueText(row.overdue_days) }}
                </td>
                <td class="ra-table__name" :data-testid="`ra-detail-bucket-${index}`">
                  <t-tag size="small" variant="light" :theme="bucketThemeOf(row.bucket)">{{ row.bucket_label }}</t-tag>
                </td>
                <td class="ra-table__num">{{ formatAmount(row.amount) }}</td>
                <td class="ra-table__num">{{ formatAmount(row.received_amount) }}</td>
                <td class="ra-table__num ra-table__num--total">{{ formatAmount(row.unsettled_amount) }}</td>
                <td class="ra-table__name">{{ statusText(row.status) }}</td>
              </tr>
              <tr v-if="!visibleDetails.length">
                <td class="ra-empty" colspan="14" data-testid="ra-detail-empty">
                  该客户在明细里没有单据（明细接口最多回 1000 张，可能被截断）
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>

      <!-- ==================== 口径提示 ==================== -->
      <ul v-if="report && report.hints.length" class="ra-hints" data-testid="ra-hints">
        <li v-for="(hint, index) in report.hints" :key="index">{{ hint }}</li>
      </ul>

      <div v-if="!report && !loading && !error" class="ra-tip" data-testid="ra-tip">
        选择期间或基准日后点「查询」，编应收账款账龄分析表（只有「未收 / 部分已收」的单据有余额）
      </div>
    </t-card>
  </div>
</template>

<script setup lang="ts">
/**
 * 应收账款账龄分析（M1-23 · 独立报表页）。
 *
 * # 唯一口径：后端 `GET /business/erp/receivable/aging`
 *
 * 页面**不算账**：分档、汇总、逾期占比、最长逾期天数、`total_unsettled` 全部由后端算好
 * （SQL 聚合，不受明细 1000 条截断影响）。前端只做四件事：
 *
 * 1. 按 `report.buckets` 渲染表头 —— 档位由后端 `AGING_BUCKETS` 定义，加档位前端自动跟上；
 * 2. 金额格式化成千分位（十进制字符串四舍五入，见 `receivable-aging-logic.formatAmount`）；
 * 3. 客户行的**显示**筛选 / 排序 —— 只改顺序与显隐，**不重算任何合计**（客户端加总 = 第二套账）；
 * 4. 点客户行 → 明细只看该客户（`detailsForCustomer`）。
 *
 * # 基准日怎么来的（与应付账龄同一套语义）
 *
 * 选期间 → 基准日自动清空 = 后端按「该期间末日」算（`as_of_source = PERIOD`）；
 * 手改基准日 → `QUERY`；两个都空 → 后端按今天（CST）算，`TODAY`。后端在 `as_of_source`
 * 里回显，页面照实写，不猜。
 *
 * # 权限（两个码，别指望一个）
 *
 * 页面码 `business:erp:report:receivable-aging`（V0418 菜单行 + 路由 `meta.permission`）、
 * 取数码 `business:erp:receivable:aging`（V0288 按钮行）。403 时页面给出点名缺哪个码的人话，
 * 而不是「无权限」四个字（见 `requestErrorMessage`）。
 */
import { computed, onMounted, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';

import {
  exportReceivableAging,
  fetchPeriodPage,
  fetchReceivableAging,
  type AccountPeriodRow,
  type ReceivableAging as ReceivableAgingData,
  type ReceivableAgingCustomer,
} from '@/api/business/erp';
import { saveBlob } from '@/utils/download';

import {
  CUSTOMER_SORT_OPTIONS,
  agingExportFileName,
  arrangeCustomers,
  asOfSourceText,
  bucketShare,
  bucketThemeOf,
  detailsForCustomer,
  detailsScopeText,
  formatAmount,
  formatDateTime,
  isZeroAmount,
  maxOverdueDays,
  overdueText,
  periodStatusText,
  periodsToOptions,
  requestErrorMessage,
  sourceTypeText,
  statusText,
  type CustomerSortMode,
} from './receivable-aging-logic';

const period = ref('');
const periodOptions = ref<{ label: string; value: string }[]>([]);
const periodLoading = ref(false);
const asOf = ref('');
const keyword = ref('');
const sortMode = ref<CustomerSortMode>('SERVER');
const loading = ref(false);
const exporting = ref(false);
const error = ref('');
const report = ref<ReceivableAgingData | null>(null);
/** 明细收窄到哪个客户（`''` = 全部客户） */
const selectedCustomerId = ref('');

/** 客户行：先按关键字筛、再按选定方式排（纯逻辑，可单测） */
const visibleCustomers = computed<ReceivableAgingCustomer[]>(() =>
  report.value ? arrangeCustomers(report.value.customers, keyword.value, sortMode.value) : [],
);

/** 关键字是否真的在筛（决定页面上那句「合计行仍是全表数」的提示） */
const filterActive = computed(() => keyword.value.trim().length > 0);

/** 明细：选中客户后只列该客户的单据 */
const visibleDetails = computed(() =>
  report.value ? detailsForCustomer(report.value.details, selectedCustomerId.value) : [],
);

/** 选中客户的名字（明细标题里点名） */
const selectedCustomerName = computed(() => {
  if (!selectedCustomerId.value || !report.value) return '';
  const hit = report.value.customers.find((item) => item.customer_id === selectedCustomerId.value);
  return hit?.customer_name ?? '';
});

/** 档位进度条配色（档位序号越靠后越红；认不出的键灰色） */
const BUCKET_COLORS: Record<string, string> = {
  NOT_DUE: '#2ba471',
  D0_30: '#0052d9',
  D31_60: '#ed7b2f',
  D61_90: '#e37318',
  D91_180: '#d54941',
  D181_365: '#b32a24',
  OVER_365: '#8b1a16',
  OVER_90: '#d54941',
};

function bucketColor(key: string): string {
  return BUCKET_COLORS[key] ?? '#c5c5c5';
}

/** 空值占位（`source_no` / `invoice_no` 可能是空串；不显示 '' 让格子塌掉） */
function dash(value: string | null | undefined): string {
  const text = (value ?? '').toString().trim();
  return text || '—';
}

/** 会计期间下拉（只列已建立期间，按期间倒序） */
async function loadPeriods() {
  periodLoading.value = true;
  try {
    const data = await fetchPeriodPage({ page: 1, page_size: 100 });
    const list = periodsToOptions((data.list ?? []) as AccountPeriodRow[]);
    periodOptions.value = list;
    if (!period.value && list.length) {
      period.value = list[0].value;
      // 基准日留空 = 后端按「该期间末日」算，页头会写「期间末日」；
      // 前端自己填一个末日再传上去，后端只能认为这是「手填」，来源就永远显示「手动指定」了。
      asOf.value = '';
    }
  } catch (err) {
    // 期间列表拉不到不影响出表：可以只按基准日查
    error.value = `期间列表加载失败：${requestErrorMessage(err)}`;
  } finally {
    periodLoading.value = false;
  }
}

async function load() {
  loading.value = true;
  error.value = '';
  try {
    report.value = await fetchReceivableAging({
      period: period.value || undefined,
      as_of: asOf.value || undefined,
    });
    // 重新出表后原来的选中客户可能已经不在这张表里（收清了 / 跨期了）→ 取消收窄，
    // 否则明细区会显示「该客户没有单据」而用户以为是少了数据。
    if (
      selectedCustomerId.value &&
      !report.value.customers.some((item) => item.customer_id === selectedCustomerId.value)
    ) {
      selectedCustomerId.value = '';
    }
  } catch (err) {
    report.value = null;
    selectedCustomerId.value = '';
    error.value = requestErrorMessage(err);
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

/** 点客户行：切换明细收窄（再点一次取消） */
function toggleCustomer(customer: ReceivableAgingCustomer) {
  selectedCustomerId.value = selectedCustomerId.value === customer.customer_id ? '' : customer.customer_id;
}

function clearSelectedCustomer() {
  selectedCustomerId.value = '';
}

async function handleExport() {
  if (!report.value) return;
  exporting.value = true;
  try {
    const blob = await exportReceivableAging({
      period: period.value || undefined,
      as_of: asOf.value || undefined,
    });
    saveBlob(blob, agingExportFileName(report.value));
    MessagePlugin.success('已导出');
  } catch (err) {
    MessagePlugin.error(`导出失败：${requestErrorMessage(err)}`);
  } finally {
    exporting.value = false;
  }
}

onMounted(async () => {
  await loadPeriods();
  await load();
});
</script>

<style scoped>
.ra-card {
  min-height: 200px;
}

.ra-tool__label {
  color: var(--td-text-color-secondary);
  font-size: 13px;
}

.ra-tool__period {
  width: 200px;
}

.ra-tool__date {
  width: 190px;
}

.ra-tool__search {
  width: 220px;
}

.ra-tool__sort {
  width: 200px;
}

.ra-tool__spacer {
  flex: 1;
}

.ra-tool__muted {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.ra-tool--sub {
  margin-top: 8px;
}

.ra-alert {
  margin: 12px 0;
}

.ra-head {
  text-align: center;
  margin: 8px 0 12px;
}

.ra-head__title {
  margin: 0 0 6px;
  font-size: 20px;
  font-weight: 600;
  letter-spacing: 2px;
}

.ra-head__meta {
  display: flex;
  justify-content: center;
  gap: 20px;
  flex-wrap: wrap;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.ra-kpi {
  display: flex;
  gap: 12px;
  flex-wrap: wrap;
  margin-bottom: 12px;
}

.ra-kpi__item {
  flex: 1 1 170px;
  padding: 10px 14px;
  border-radius: 6px;
  background: var(--td-bg-color-secondarycontainer);
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.ra-kpi__label {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.ra-kpi__value {
  font-size: 18px;
  font-weight: 600;
}

.ra-kpi__value--warn {
  color: var(--td-error-color);
}

.ra-section__title {
  font-weight: 600;
  margin-bottom: 8px;
}

.ra-section__note {
  margin-left: 10px;
  font-weight: 400;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.ra-body {
  overflow-x: auto;
}

.ra-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
}

.ra-table th,
.ra-table td {
  border: 1px solid var(--td-component-border);
  padding: 6px 10px;
  line-height: 1.6;
}

.ra-table thead th {
  background: var(--td-bg-color-secondarycontainer);
  font-weight: 600;
  text-align: center;
  white-space: nowrap;
}

.ra-table__name {
  text-align: left;
}

.ra-table__num {
  text-align: right;
  font-variant-numeric: tabular-nums;
}

.ra-table__num--total {
  font-weight: 600;
}

.ra-row--total td {
  background: var(--td-bg-color-container-select, #f3f6fb);
  font-weight: 600;
}

.ra-row--clickable {
  cursor: pointer;
}

.ra-row--clickable:hover td {
  background: var(--td-bg-color-container-hover, #f9fbfd);
}

.ra-row--selected td {
  background: var(--td-brand-color-light, #eef3ff);
}

.ra-cell--zero {
  color: var(--td-text-color-placeholder);
}

.ra-cell--overdue {
  color: var(--td-error-color);
  font-weight: 600;
}

.ra-cell__bar {
  display: block;
  height: 3px;
  margin-top: 3px;
  border-radius: 2px;
  background: var(--td-bg-color-component, #eee);
  overflow: hidden;
}

.ra-cell__bar i {
  display: block;
  height: 100%;
}

.ra-empty {
  text-align: center;
  color: var(--td-text-color-placeholder);
  padding: 24px 0;
}

.ra-detail {
  margin-top: 20px;
}

.ra-detail__title {
  font-weight: 600;
  margin-bottom: 8px;
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.ra-detail__scope {
  font-weight: 400;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.ra-table--detail {
  font-size: 12px;
}

.ra-hints {
  margin: 12px 0 0;
  padding-left: 18px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 1.8;
}

.ra-tip {
  color: var(--td-text-color-placeholder);
  text-align: center;
  padding: 40px 0;
}
</style>

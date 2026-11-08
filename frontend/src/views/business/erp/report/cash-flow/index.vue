<template>
  <div class="cf-page">
    <t-card :bordered="false" class="cf-card">
      <!-- ==================== 工具栏：期间（必选）+ 查询 + 导出 ==================== -->
      <div class="tool-row">
        <span class="tool-row__label">期间</span>
        <t-select
          v-model="period"
          class="tool-row__period"
          :options="periodOptions"
          :loading="periodLoading"
          placeholder="选择会计期间"
          data-testid="cf-period-select"
          @change="handlePeriodChange"
        />
        <t-button theme="primary" :loading="loading" data-testid="cf-search" @click="load()">查询</t-button>
        <t-button variant="outline" :disabled="!sheet" data-testid="cf-export" @click="handleExport">
          <template #icon><t-icon name="download" /></template>
          导出 Excel
        </t-button>
        <DocPdfButton
          size="medium"
          :disabled="!sheet"
          :path="`/api/business/erp/report/cash-flow/pdf?period=${encodeURIComponent(sheet?.period ?? '')}`"
          :filename="`现金流量表_${sheet?.period ?? ''}.pdf`"
          testid="cf-export-pdf"
        />
        <span v-if="periodHint" class="tool-row__hint" data-testid="cf-period-hint">{{ periodHint }}</span>
      </div>

      <t-alert v-if="error" theme="error" :message="error" class="cf-alert" data-testid="cf-error">
        <template #operation>
          <t-button size="small" variant="outline" @click="load()">重试</t-button>
        </template>
      </t-alert>

      <!-- ==================== 口径 / 未分类告警（黄条） ==================== -->
      <t-alert
        v-if="sheet && sheet.warnings.length"
        theme="warning"
        class="cf-alert"
        data-testid="cf-warn"
      >
        <template #message>
          <div class="cf-warn__title">本表有 {{ sheet.warnings.length }} 条告警（不静默丢：未分类的收支已归「其他」）</div>
          <ul class="cf-warn__list">
            <li v-for="(warning, index) in sheet.warnings" :key="index" data-testid="cf-warn-item">{{ warning }}</li>
          </ul>
        </template>
      </t-alert>

      <!-- ==================== 表头：编制单位 / 期间 / 单位 / 累计起点 ==================== -->
      <div v-if="sheet" class="cf-head" data-testid="cf-head">
        <h2 class="cf-head__title">{{ sheet.report_name }}</h2>
        <div class="cf-head__meta">
          <span data-testid="cf-company">编制单位：{{ sheet.company_name }}</span>
          <span data-testid="cf-period-name">期间：{{ sheet.period_name }}（{{ periodStatusText(sheet.period_status) }}）</span>
          <span>单位：{{ sheet.unit }}</span>
          <span data-testid="cf-ytd-start">本年累计：{{ sheet.ytd_start }} 起</span>
          <span data-testid="cf-generated-at">编制时间：{{ formatDateTime(sheet.generated_at) }}</span>
        </div>
        <div class="cf-head__note" data-testid="cf-simplified">口径（简化版）：{{ sheet.simplified_note }}</div>
      </div>

      <!-- ==================== 报表主体 ==================== -->
      <t-loading :loading="loading" size="small">
        <div v-if="sheet" class="cf-body" data-testid="cf-table">
          <table class="cf-table">
            <thead>
              <tr>
                <th class="cf-table__item">项目</th>
                <th class="cf-table__num">{{ sheet.current_label }}</th>
                <th class="cf-table__num">{{ sheet.ytd_label }}</th>
              </tr>
            </thead>
            <tbody>
              <template v-for="section in sections" :key="section.activity">
                <tr class="cf-row--activity" :data-testid="`cf-section-${section.activity}`">
                  <td class="cf-table__item">{{ section.activity_name }}</td>
                  <td class="cf-table__num">{{ formatAmount(section.net.current) }}</td>
                  <td class="cf-table__num">{{ formatAmount(section.net.ytd) }}</td>
                </tr>

                <tr
                  v-for="line in section.inflow"
                  :key="`${section.activity}-in-${line.item_code}`"
                  class="cf-row--line"
                  :data-testid="`cf-line-${line.item_code}`"
                >
                  <td class="cf-table__item"><span class="cf-indent">{{ line.item_name }}</span></td>
                  <td class="cf-table__num" :data-testid="`cf-current-${line.item_code}`">{{ formatAmount(line.current) }}</td>
                  <td class="cf-table__num" :data-testid="`cf-ytd-${line.item_code}`">{{ formatAmount(line.ytd) }}</td>
                </tr>
                <tr class="cf-row--subtotal" :data-testid="`cf-inflow-total-${section.activity}`">
                  <td class="cf-table__item"><span class="cf-indent">{{ activityLabel(section.activity) }}现金流入小计</span></td>
                  <td class="cf-table__num">{{ formatAmount(section.inflow_total.current) }}</td>
                  <td class="cf-table__num">{{ formatAmount(section.inflow_total.ytd) }}</td>
                </tr>

                <tr
                  v-for="line in section.outflow"
                  :key="`${section.activity}-out-${line.item_code}`"
                  class="cf-row--line"
                  :data-testid="`cf-line-${line.item_code}`"
                >
                  <td class="cf-table__item"><span class="cf-indent">{{ line.item_name }}</span></td>
                  <td class="cf-table__num" :data-testid="`cf-current-${line.item_code}`">{{ formatAmount(line.current) }}</td>
                  <td class="cf-table__num" :data-testid="`cf-ytd-${line.item_code}`">{{ formatAmount(line.ytd) }}</td>
                </tr>
                <tr class="cf-row--subtotal" :data-testid="`cf-outflow-total-${section.activity}`">
                  <td class="cf-table__item"><span class="cf-indent">{{ activityLabel(section.activity) }}现金流出小计</span></td>
                  <td class="cf-table__num">{{ formatAmount(section.outflow_total.current) }}</td>
                  <td class="cf-table__num">{{ formatAmount(section.outflow_total.ytd) }}</td>
                </tr>

                <tr class="cf-row--net" :data-testid="`cf-net-${section.activity}`">
                  <td class="cf-table__item">{{ activityLabel(section.activity) }}产生的现金流量净额</td>
                  <td class="cf-table__num" :data-testid="`cf-net-current-${section.activity}`">{{ formatAmount(section.net.current) }}</td>
                  <td class="cf-table__num" :data-testid="`cf-net-ytd-${section.activity}`">{{ formatAmount(section.net.ytd) }}</td>
                </tr>
              </template>

              <tr class="cf-row--total" data-testid="cf-net-increase">
                <td class="cf-table__item">现金及现金等价物净增加额</td>
                <td class="cf-table__num" data-testid="cf-net-increase-current">{{ formatAmount(sheet.net_increase.current) }}</td>
                <td class="cf-table__num" data-testid="cf-net-increase-ytd">{{ formatAmount(sheet.net_increase.ytd) }}</td>
              </tr>
              <tr class="cf-row--total" data-testid="cf-opening">
                <td class="cf-table__item">加：期初现金及现金等价物余额</td>
                <td class="cf-table__num" data-testid="cf-opening-current">{{ formatAmount(sheet.opening_balance.current) }}</td>
                <td class="cf-table__num" data-testid="cf-opening-ytd">{{ formatAmount(sheet.opening_balance.ytd) }}</td>
              </tr>
              <tr class="cf-row--total" data-testid="cf-closing">
                <td class="cf-table__item">期末现金及现金等价物余额</td>
                <td class="cf-table__num" data-testid="cf-closing-current">{{ formatAmount(sheet.closing_balance.current) }}</td>
                <td class="cf-table__num" data-testid="cf-closing-ytd">{{ formatAmount(sheet.closing_balance.ytd) }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </t-loading>

      <!-- ==================== 勾稽结论：期末现金 vs 资产负债表货币资金 ==================== -->
      <div
        v-if="sheet"
        :class="['cf-check', sheet.check.balanced ? 'is-check--ok' : 'is-check--bad']"
        data-testid="cf-check"
      >
        <div class="cf-check__line" data-testid="cf-check-line">
          <span class="cf-check__icon">{{ sheet.check.balanced ? '✅' : '⚠️' }}</span>
          {{ sheet.check.message }}
        </div>
        <div class="cf-check__numbers">
          <span>期末现金余额 <b data-testid="cf-check-closing">{{ formatAmount(sheet.check.closing_balance) }}</b></span>
          <span>资产负债表「货币资金」 <b data-testid="cf-check-bs-cash">{{ formatAmount(sheet.check.balance_sheet_cash) }}</b></span>
          <span>差额 <b data-testid="cf-check-diff">{{ sheet.check.diff_text }}</b></span>
          <span>总账现金类科目余额 <b data-testid="cf-check-ledger">{{ formatAmount(sheet.check.closing_by_ledger) }}</b></span>
          <span>本期现金分录 <b>{{ sheet.check.cash_entry_count }}</b> 笔</span>
          <span data-testid="cf-check-unclassified">未分类 <b>{{ sheet.check.unclassified_count }}</b> 笔</span>
        </div>
        <ul v-if="sheet.check.hints.length" class="cf-check__hints">
          <li v-for="(hint, index) in sheet.check.hints" :key="index">{{ hint }}</li>
        </ul>
        <div v-if="sheet.check.unbalanced_vouchers.length" class="cf-check__vouchers" data-testid="cf-unbalanced">
          <div class="cf-check__vouchers-title">分录合计与单头不符的凭证（{{ sheet.check.unbalanced_vouchers.length }} 张）：</div>
          <div v-for="voucher in sheet.check.unbalanced_vouchers" :key="voucher.voucher_no" class="cf-check__voucher">
            凭证 {{ voucher.voucher_no }}（{{ voucher.voucher_date }}）：分录 借 {{ formatAmount(voucher.entry_debit) }} /
            贷 {{ formatAmount(voucher.entry_credit) }}，单头 借 {{ formatAmount(voucher.header_debit) }} /
            贷 {{ formatAmount(voucher.header_credit) }}
          </div>
        </div>
      </div>

      <div v-if="!sheet && !loading && !error" class="cf-empty">
        选择期间后点「查询」编制现金流量表
      </div>
    </t-card>
  </div>
</template>

<script setup lang="ts">
/**
 * 现金流量表（ERP-R3，**简化版**）。
 *
 * # 唯一口径：后端 `/business/erp/report/cash-flow`
 *
 * 页面**不做任何金额计算**：经营 / 投资 / 筹资各自的小计与净额、净增加额、
 * 期初 / 期末余额、以及「期末现金 vs 资产负债表货币资金」的勾稽差额，全部由后端算好
 * （`check.diff` 精确到 4 位，后端明确「不许四舍五入抹平」）。前端只做千分位格式化。
 *
 * # 为什么是简化版
 *
 * 完整版要会计在每张凭证上手工指定「现金流量项目」，工作量翻倍且极易漏录。
 * 本表直接读凭证：现金类科目（1001 / 1002）的收支流水 + 对方科目自动分三类。
 * 分类不出来的收支归「其他」并在顶部黄条点名（后端 `warnings`），绝不静默丢。
 *
 * # 为什么金额是字符串
 *
 * `numeric(20,4)` → `rust_decimal` → JSON 字符串（`"16500.0000"`）。
 * 用 `Number()` 解析会引入浮点误差，报表金额不能这么玩。
 */
import { computed, onMounted, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';

import {
  exportCashFlow,
  fetchCashFlow,
  fetchPeriodPage,
  type AccountPeriodRow,
  type CashFlow as CashFlowData,
  type CashFlowSection,
} from '@/api/business/erp';
import { saveBlob } from '@/utils/download';
import DocPdfButton from '@/components/DocPdfButton.vue';

const period = ref('');
const periodOptions = ref<{ label: string; value: string }[]>([]);
const periodLoading = ref(false);
const periodHint = ref('');
const loading = ref(false);
const error = ref('');
const sheet = ref<CashFlowData | null>(null);

/** 四个分区（「其他」只在真有未分类收支时才展示，避免空表噪音） */
const sections = computed<CashFlowSection[]>(() => {
  const data = sheet.value;
  if (!data) return [];
  const list = [data.operating, data.investing, data.financing];
  const other = data.other;
  const hasOther =
    other.inflow.length > 0 || other.outflow.length > 0 || !isZeroPair(other.net);
  return hasOther ? [...list, other] : list;
});

function isZeroPair(pair: { current: string; ytd: string }): boolean {
  return Number(pair.current) === 0 && Number(pair.ytd) === 0;
}

/**
 * 千分位 + 2 位小数（`null` / 空串显示空）。
 *
 * 不用 `Number()` 做四舍五入（二进制浮点会把 0.595 舍成 0.59）：按十进制字符串
 * 看第 3 位小数决定是否对「整数分」+1（带进位），与资产负债表 / 利润表同一个写法。
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

/** 分区的中文简称（`经营活动` / `投资活动` / `筹资活动` / `未分类`） */
function activityLabel(activity: string): string {
  switch (activity) {
    case 'OPERATING':
      return '经营活动';
    case 'INVESTING':
      return '投资活动';
    case 'FINANCING':
      return '筹资活动';
    default:
      return '未分类';
  }
}

/** 期间状态的中文（`DERIVED` 是报表侧独有的「未开账·按自然月推算」） */
function periodStatusText(status: string): string {
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

/** 会计期间下拉：只列「已建立」的期间（NOT_OPENED 的不给选，选了也一定 400） */
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
      periodHint.value =
        list.length > 1
          ? ''
          : '下拉只列已开账期间；某月有凭证却没列出 = 该月还没开账，去「会计期间」开启后即可选';
    }
  } catch (err) {
    periodHint.value = `期间列表加载失败：${(err as Error).message ?? err}`;
  } finally {
    periodLoading.value = false;
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
    sheet.value = await fetchCashFlow(period.value);
  } catch (err) {
    // 后端 400/500 的 msg 已经是人话（含差多少 / 哪个凭证），直接用
    sheet.value = null;
    error.value = (err as Error).message ?? String(err);
  } finally {
    loading.value = false;
  }
}

function handlePeriodChange() {
  void load();
}

async function handleExport() {
  if (!sheet.value) return;
  try {
    const blob = await exportCashFlow(sheet.value.period);
    saveBlob(blob, `现金流量表_${sheet.value.period}.xlsx`);
    MessagePlugin.success('已导出');
  } catch (err) {
    MessagePlugin.error(`导出失败：${(err as Error).message ?? err}`);
  }
}

/** 导出 PDF（PDF-07 后端版式；PDF-08 起统一走 DocPdfButton：loading + 403 提示 + 后端中文名） */

onMounted(async () => {
  await loadPeriods();
  if (period.value) {
    await load();
  }
});
</script>

<style scoped>
.cf-page {
  padding: 0;
}

.cf-card {
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
  width: 220px;
}

.tool-row__hint {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.cf-alert {
  margin-bottom: 12px;
}

.cf-warn__title {
  font-weight: 600;
  margin-bottom: 4px;
}

.cf-warn__list {
  margin: 0;
  padding-left: 18px;
  line-height: 1.7;
}

.cf-head {
  text-align: center;
  margin: 8px 0 12px;
}

.cf-head__title {
  margin: 0 0 6px;
  font-size: 20px;
  font-weight: 600;
  letter-spacing: 2px;
}

.cf-head__meta {
  display: flex;
  justify-content: center;
  gap: 20px;
  flex-wrap: wrap;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.cf-head__note {
  margin-top: 6px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
  line-height: 1.6;
}

.cf-body {
  overflow-x: auto;
}

.cf-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
}

.cf-table th,
.cf-table td {
  border: 1px solid var(--td-component-border);
  padding: 6px 10px;
  line-height: 1.6;
}

.cf-table thead th {
  background: var(--td-bg-color-secondarycontainer);
  font-weight: 600;
  text-align: center;
}

.cf-table__item {
  width: 56%;
  text-align: left;
}

.cf-table__num {
  width: 22%;
  text-align: right;
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
}

.cf-indent {
  padding-left: 16px;
}

.cf-row--activity td {
  font-weight: 600;
  background: var(--td-bg-color-secondarycontainer);
}

.cf-row--subtotal td {
  font-weight: 600;
}

.cf-row--net td {
  font-weight: 600;
  background: var(--td-bg-color-container-hover);
}

.cf-row--total td {
  font-weight: 600;
  background: var(--td-bg-color-container-active);
}

.cf-check {
  margin-top: 14px;
  padding: 12px 14px;
  border-radius: 6px;
  font-size: 13px;
}

.is-check--ok {
  background: var(--td-success-color-1);
  border: 1px solid var(--td-success-color-2);
  color: var(--td-success-color-7, #2ba471);
}

.is-check--bad {
  background: var(--td-error-color-1);
  border: 1px solid var(--td-error-color-2);
  color: var(--td-error-color-7, #d54941);
}

.cf-check__line {
  font-size: 15px;
  font-weight: 600;
  margin-bottom: 6px;
}

.cf-check__icon {
  margin-right: 6px;
}

.cf-check__numbers {
  display: flex;
  gap: 18px;
  flex-wrap: wrap;
  font-variant-numeric: tabular-nums;
}

.cf-check__hints {
  margin: 8px 0 0;
  padding-left: 20px;
  line-height: 1.7;
}

.cf-check__vouchers {
  margin-top: 8px;
  line-height: 1.7;
}

.cf-check__vouchers-title {
  font-weight: 600;
}

.cf-empty {
  padding: 40px 0;
  text-align: center;
  color: var(--td-text-color-placeholder);
}
</style>

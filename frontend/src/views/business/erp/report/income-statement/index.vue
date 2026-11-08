<template>
  <div class="is-page">
    <t-card :bordered="false" class="is-card">
      <!-- ==================== 工具栏：期间（必选）+ 查询 + 导出 ==================== -->
      <div class="tool-row">
        <span class="tool-row__label">期间</span>
        <t-select
          v-model="period"
          class="tool-row__period"
          :options="periodOptions"
          :loading="periodLoading"
          placeholder="选择会计期间"
          data-testid="is-period-select"
          @change="handlePeriodChange"
        />
        <t-button theme="primary" :loading="loading" data-testid="is-search" @click="load()">查询</t-button>
        <t-button variant="outline" :disabled="!sheet" data-testid="is-export" @click="handleExport">
          <template #icon><t-icon name="download" /></template>
          导出 Excel
        </t-button>
        <DocPdfButton
          size="medium"
          :disabled="!sheet"
          :path="`/api/business/erp/report/income-statement/pdf?period=${encodeURIComponent(sheet?.period ?? '')}`"
          :filename="`利润表_${sheet?.period ?? ''}.pdf`"
          testid="is-export-pdf"
        />
        <span v-if="periodHint" class="tool-row__hint" data-testid="is-period-hint">{{ periodHint }}</span>
      </div>

      <t-alert v-if="error" theme="error" :message="error" class="is-alert" data-testid="is-error">
        <template #operation>
          <t-button size="small" variant="outline" @click="load()">重试</t-button>
        </template>
      </t-alert>

      <!-- ==================== 表头：编制单位 / 期间 / 单位 / 累计起点 ==================== -->
      <div v-if="sheet" class="is-head" data-testid="is-head">
        <h2 class="is-head__title">{{ sheet.report_name }}</h2>
        <div class="is-head__meta">
          <span data-testid="is-company">编制单位：{{ sheet.company_name }}</span>
          <span data-testid="is-period-name">期间：{{ sheet.period_name }}（{{ periodStatusText(sheet.period_status) }}）</span>
          <span>单位：{{ sheet.unit }}</span>
          <span data-testid="is-ytd-start">本年累计：{{ sheet.ytd_start }} 起</span>
          <span data-testid="is-generated-at">编制时间：{{ formatDateTime(sheet.generated_at) }}</span>
        </div>
      </div>

      <!-- ==================== 报表主体：项目 / 本期金额 / 本年累计 ==================== -->
      <t-loading :loading="loading" size="small">
        <div v-if="sheet" class="is-body" data-testid="is-table">
          <table class="is-table">
            <thead>
              <tr>
                <th class="is-table__item">项目</th>
                <th class="is-table__num">{{ sheet.current_label }}</th>
                <th class="is-table__num">{{ sheet.ytd_label }}</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="(row, index) in sheet.items"
                :key="row.item_code"
                :class="[
                  'is-row',
                  `is-row--l${row.level}`,
                  { 'is-row--total': row.is_total, 'is-row--unmapped': row.item_code === UNMAPPED_CODE },
                ]"
                :data-testid="`is-row-${index}`"
              >
                <td class="is-table__item">
                  <span :style="indent(row.level)">{{ row.item_name }}</span>
                </td>
                <td class="is-table__num" :data-testid="`is-current-${row.item_code}`">
                  {{ formatAmount(row.current) }}
                </td>
                <td class="is-table__num" :data-testid="`is-ytd-${row.item_code}`">
                  {{ formatAmount(row.ytd) }}
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </t-loading>

      <!-- ==================== 勾稽结论：一致绿 / 不平红 ==================== -->
      <div
        v-if="sheet"
        :class="['is-check', sheet.check.balanced ? 'is-check--ok' : 'is-check--bad']"
        data-testid="is-check"
      >
        <div class="is-check__line" data-testid="is-check-line">
          <span class="is-check__icon">{{ sheet.check.balanced ? '✅' : '⚠️' }}</span>
          {{ sheet.check.message }}
        </div>
        <div class="is-check__numbers">
          <span>净利润（本年累计） <b data-testid="is-net-profit-ytd">{{ formatAmount(sheet.check.net_profit_ytd) }}</b></span>
          <span>资产负债表「未分配利润」本年变动 <b data-testid="is-equity-change">{{ formatAmount(sheet.check.equity_change) }}</b></span>
          <span>差额 <b data-testid="is-check-diff">{{ sheet.check.diff_text }}</b></span>
          <span>
            未分配利润：期末 <b>{{ formatAmount(sheet.check.undistributed_profit_end) }}</b> / 年初
            <b>{{ formatAmount(sheet.check.undistributed_profit_year_start) }}</b>
          </span>
          <span v-if="!isZero(sheet.check.unmapped_profit_loss_ytd)" data-testid="is-unmapped-total">
            未归类损益 <b>{{ formatAmount(sheet.check.unmapped_profit_loss_ytd) }}</b>
          </span>
        </div>
        <ul v-if="sheet.check.hints.length" class="is-check__hints">
          <li v-for="(hint, index) in sheet.check.hints" :key="index">{{ hint }}</li>
        </ul>
        <div v-if="sheet.check.unmapped.length" class="is-check__unmapped" data-testid="is-unmapped-list">
          <div class="is-check__unmapped-title">没有归入利润表任何一行的损益类科目（{{ sheet.check.unmapped.length }} 个）：</div>
          <div v-for="row in sheet.check.unmapped" :key="row.account_code" class="is-check__unmapped-row">
            科目 {{ row.account_code }} {{ row.account_name }}：本期 {{ formatAmount(row.current) }} /
            本年累计 {{ formatAmount(row.ytd) }}（影响利润 {{ formatAmount(row.effect_ytd) }}）
          </div>
        </div>
        <div v-if="sheet.check.unbalanced_vouchers.length" class="is-check__vouchers">
          <div class="is-check__vouchers-title">分录合计与单头不符的凭证（{{ sheet.check.unbalanced_vouchers.length }} 张）：</div>
          <div v-for="voucher in sheet.check.unbalanced_vouchers" :key="voucher.voucher_no" class="is-check__voucher">
            凭证 {{ voucher.voucher_no }}（{{ voucher.voucher_date }}）：分录 借 {{ formatAmount(voucher.entry_debit) }} /
            贷 {{ formatAmount(voucher.entry_credit) }}，单头 借 {{ formatAmount(voucher.header_debit) }} /
            贷 {{ formatAmount(voucher.header_credit) }}
          </div>
        </div>
      </div>

      <div v-if="!sheet && !loading && !error" class="is-empty">
        选择期间后点「查询」编制利润表
      </div>
    </t-card>
  </div>
</template>

<script setup lang="ts">
/**
 * 利润表（ERP-R2）。
 *
 * # 唯一口径：后端 `/business/erp/report/income-statement`
 *
 * 页面**不做任何金额计算**：营业收入 / 营业成本 / 三项期间费用 / 营业利润 / 利润总额 /
 * 净利润、以及「净利润（本年累计）vs 资产负债表未分配利润变动」的勾稽差额，
 * 全部由后端算好（`check.diff` 精确到 4 位，后端明确「不许四舍五入抹平」）。
 * 前端只把字符串金额格式化成千分位。
 *
 * # 为什么金额是字符串
 *
 * `numeric(20,4)` → `rust_decimal` → JSON 字符串（`"16500.0000"`）。
 * 用 `Number()` 解析会引入浮点误差（0.1 + 0.2 那类），报表的金额不能这么玩。
 *
 * # 两列的含义（最容易看错的地方）
 *
 * - **本期金额**：`voucher_date` 落在本期期间内的发生额；
 * - **本年累计**：1 月 1 日到本期期末的发生额（不是「本季」也不是「上月」）。
 *
 * # 两个已知的「预期失败」
 *
 * - `period` 没在「会计期间」里建立且该月没有凭证 → 400 + `66010`；
 * - `period` 格式不对 → 400 + `10001`。
 * 两者都直接显示后端返回的 `msg`，不自己编文案。
 */
import { onMounted, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';

import {
  exportIncomeStatement,
  fetchIncomeStatement,
  fetchPeriodPage,
  type AccountPeriodRow,
  type IncomeStatement as IncomeStatementData,
} from '@/api/business/erp';
import { saveBlob } from '@/utils/download';
import DocPdfButton from '@/components/DocPdfButton.vue';

/** 运行时补出来的「未归类损益」行编码（不落库，后端拼出来的） */
const UNMAPPED_CODE = 'UNMAPPED_PROFIT_LOSS';

const period = ref('');
const periodOptions = ref<{ label: string; value: string }[]>([]);
const periodLoading = ref(false);
const periodHint = ref('');
const loading = ref(false);
const error = ref('');
const sheet = ref<IncomeStatementData | null>(null);

/**
 * 千分位 + 2 位小数（`null` / 空串显示空）。
 *
 * **不用 `Number()` 做四舍五入**：`Number('0.595').toFixed(2)` 会得到 `0.59`（二进制浮点误差），
 * 报表上少一分钱是事故。这里按十进制字符串做「四舍五入到分」——
 * 看第 3 位小数，够 5 就对「整数分」这个字符串 +1（带进位）。
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
    // 十进制串 +1（分），逐位进位
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

/** 金额是否为零（用于决定要不要显示「未归类损益」这类只在异常时才有意义的信息） */
function isZero(value: string | null | undefined): boolean {
  if (value === null || value === undefined || value === '') return true;
  return Number(value) === 0;
}

/** 层级缩进（1 大类不缩进、2 明细缩一级） */
function indent(level: number): string {
  return `padding-left: ${Math.max(0, level - 1) * 16}px`;
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
    sheet.value = await fetchIncomeStatement(period.value);
  } catch (err) {
    // 后端 400/500 的 msg 已经是人话（含「差多少」「哪个科目」），直接用
    sheet.value = null;
    error.value = (err as Error).message ?? String(err);
  } finally {
    loading.value = false;
  }
}

/** 期间切换后自动重算（避免「选了期间但要再点一次查询」的错觉） */
function handlePeriodChange() {
  void load();
}

async function handleExport() {
  if (!sheet.value) return;
  try {
    const blob = await exportIncomeStatement(sheet.value.period);
    saveBlob(blob, `利润表_${sheet.value.period}.xlsx`);
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
.is-page {
  padding: 0;
}

.is-card {
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

.is-alert {
  margin-bottom: 12px;
}

.is-head {
  text-align: center;
  margin: 8px 0 12px;
}

.is-head__title {
  margin: 0 0 6px;
  font-size: 20px;
  font-weight: 600;
  letter-spacing: 2px;
}

.is-head__meta {
  display: flex;
  justify-content: center;
  gap: 20px;
  flex-wrap: wrap;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.is-body {
  overflow-x: auto;
}

.is-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
}

.is-table th,
.is-table td {
  border: 1px solid var(--td-component-border);
  padding: 6px 10px;
  line-height: 1.6;
}

.is-table thead th {
  background: var(--td-bg-color-secondarycontainer);
  font-weight: 600;
  text-align: center;
}

.is-table__item {
  width: 56%;
  text-align: left;
}

.is-table__num {
  width: 22%;
  text-align: right;
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
}

/* 层级：1 大类加粗、2 明细正常 */
.is-row--l1 td {
  font-weight: 600;
}

/* 合计行（营业利润 / 利润总额 / 净利润）再加重一档 + 底色 */
.is-row--total td {
  font-weight: 600;
  background: var(--td-bg-color-container-hover);
}

/* 未归类损益是告警行，不是正常报表行 */
.is-row--unmapped td {
  color: var(--td-warning-color-7, #b25e09);
  font-style: italic;
}

.is-check {
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

.is-check__line {
  font-size: 15px;
  font-weight: 600;
  margin-bottom: 6px;
}

.is-check__icon {
  margin-right: 6px;
}

.is-check__numbers {
  display: flex;
  gap: 18px;
  flex-wrap: wrap;
  font-variant-numeric: tabular-nums;
}

.is-check__hints {
  margin: 8px 0 0;
  padding-left: 20px;
  line-height: 1.7;
}

.is-check__unmapped,
.is-check__vouchers {
  margin-top: 8px;
  line-height: 1.7;
}

.is-check__unmapped-title,
.is-check__vouchers-title {
  font-weight: 600;
}

.is-empty {
  padding: 40px 0;
  text-align: center;
  color: var(--td-text-color-placeholder);
}
</style>

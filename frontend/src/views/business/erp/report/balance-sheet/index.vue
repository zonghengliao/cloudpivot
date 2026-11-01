<template>
  <div class="bs-page">
    <t-card :bordered="false" class="bs-card">
      <!-- ==================== 工具栏：期间（必选）+ 查询 + 导出 ==================== -->
      <div class="tool-row">
        <span class="tool-row__label">期间</span>
        <t-select
          v-model="period"
          class="tool-row__period"
          :options="periodOptions"
          :loading="periodLoading"
          placeholder="选择会计期间"
          data-testid="bs-period-select"
          @change="handlePeriodChange"
        />
        <t-button theme="primary" :loading="loading" data-testid="bs-search" @click="load()">查询</t-button>
        <t-button variant="outline" :disabled="!sheet" data-testid="bs-export" @click="handleExport">
          <template #icon><t-icon name="download" /></template>
          导出 Excel
        </t-button>
        <span v-if="periodHint" class="tool-row__hint" data-testid="bs-period-hint">{{ periodHint }}</span>
      </div>

      <t-alert v-if="error" theme="error" :message="error" class="bs-alert" data-testid="bs-error">
        <template #operation>
          <t-button size="small" variant="outline" @click="load()">重试</t-button>
        </template>
      </t-alert>

      <!-- ==================== 表头：编制单位 / 期间 / 单位 / 编制时间 ==================== -->
      <div v-if="sheet" class="bs-head" data-testid="bs-head">
        <h2 class="bs-head__title">{{ sheet.report_name }}</h2>
        <div class="bs-head__meta">
          <span data-testid="bs-company">编制单位：{{ sheet.company_name }}</span>
          <span data-testid="bs-period-name">期间：{{ sheet.period_name }}（{{ periodStatusText(sheet.period_status) }}）</span>
          <span>单位：{{ sheet.unit }}</span>
          <span data-testid="bs-generated-at">编制时间：{{ formatDateTime(sheet.generated_at) }}</span>
          <span v-if="sheet.previous_period" data-testid="bs-previous-period">上期：{{ sheet.previous_period }}</span>
        </div>
      </div>

      <!-- ==================== 报表主体：左资产 / 右负债和权益 ==================== -->
      <t-loading :loading="loading" size="small">
        <div v-if="sheet" class="bs-body" data-testid="bs-table">
          <table class="bs-table">
            <thead>
              <tr>
                <th class="bs-table__item">资产</th>
                <th class="bs-table__num">期末余额</th>
                <th class="bs-table__num bs-table__num--prev">上期余额</th>
                <th class="bs-table__item bs-table__item--right">负债和所有者权益</th>
                <th class="bs-table__num">期末余额</th>
                <th class="bs-table__num bs-table__num--prev">上期余额</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="(row, index) in rows"
                :key="index"
                :class="['bs-row', `bs-row--l${row.left?.level ?? row.right?.level ?? 2}`, { 'bs-row--total': row.total }]"
                :data-testid="`bs-row-${index}`"
              >
                <!-- 左：资产 -->
                <td :class="itemCellClass(row.left)">
                  <span v-if="row.left" :style="indent(row.left.level)">
                    {{ row.left ? itemLabel(row.left) : '' }}
                  </span>
                </td>
                <td class="bs-table__num" :data-testid="row.left ? `bs-amount-${row.left.item_code}` : undefined">
                  {{ row.left ? formatAmount(row.left.amount) : '' }}
                </td>
                <td class="bs-table__num bs-table__num--prev">{{ row.left ? formatAmount(row.left.previous_amount) : '' }}</td>

                <!-- 右：负债和所有者权益 -->
                <td :class="itemCellClass(row.right, true)">
                  <span v-if="row.right" :style="indent(row.right.level)">
                    {{ row.right ? itemLabel(row.right) : '' }}
                  </span>
                </td>
                <td class="bs-table__num" :data-testid="row.right ? `bs-amount-${row.right.item_code}` : undefined">
                  {{ row.right ? formatAmount(row.right.amount) : '' }}
                </td>
                <td class="bs-table__num bs-table__num--prev">{{ row.right ? formatAmount(row.right.previous_amount) : '' }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </t-loading>

      <!-- ==================== 勾稽结论：平绿 / 不平红 ==================== -->
      <div
        v-if="sheet"
        :class="['bs-check', sheet.check.balanced ? 'bs-check--ok' : 'bs-check--bad']"
        data-testid="bs-check"
      >
        <div class="bs-check__line" data-testid="bs-check-line">
          <span class="bs-check__icon">{{ sheet.check.balanced ? '✅' : '⚠️' }}</span>
          {{ sheet.check.message }}
        </div>
        <div class="bs-check__numbers">
          <span>资产总计 <b>{{ formatAmount(sheet.check.assets_total) }}</b></span>
          <span>负债+权益 <b>{{ formatAmount(sheet.check.liability_equity_total) }}</b></span>
          <span>差额 <b data-testid="bs-check-diff">{{ sheet.check.diff_text }}</b></span>
          <span>本期 Σ借 <b>{{ formatAmount(sheet.check.ledger_debit_total) }}</b> / Σ贷 <b>{{ formatAmount(sheet.check.ledger_credit_total) }}</b></span>
        </div>
        <ul v-if="sheet.check.hints.length" class="bs-check__hints">
          <li v-for="(hint, index) in sheet.check.hints" :key="index">{{ hint }}</li>
        </ul>
        <div v-if="sheet.check.unbalanced_vouchers.length" class="bs-check__vouchers">
          <div class="bs-check__vouchers-title">分录合计与单头不符的凭证（{{ sheet.check.unbalanced_vouchers.length }} 张）：</div>
          <div v-for="voucher in sheet.check.unbalanced_vouchers" :key="voucher.voucher_no" class="bs-check__voucher">
            凭证 {{ voucher.voucher_no }}（{{ voucher.voucher_date }}）：分录 借 {{ formatAmount(voucher.entry_debit) }} /
            贷 {{ formatAmount(voucher.entry_credit) }}，单头 借 {{ formatAmount(voucher.header_debit) }} /
            贷 {{ formatAmount(voucher.header_credit) }}
          </div>
        </div>
      </div>

      <div v-if="!sheet && !loading && !error" class="bs-empty">
        选择期间后点「查询」编制资产负债表
      </div>
    </t-card>
  </div>
</template>

<script setup lang="ts">
/**
 * 资产负债表（ERP-R1）。
 *
 * # 唯一口径：后端 `/business/erp/report/balance-sheet`
 *
 * 页面**不做任何金额计算**，只负责排版：
 * 合计、小计、减项（累计折旧）、勾稽差额全部由后端算好（`check.diff` 精确到 4 位小数，
 * 后端明确"不许四舍五入抹平"）。前端只把字符串金额格式化成人看的千分位。
 *
 * # 为什么金额是字符串
 *
 * `numeric(20,4)` → `rust_decimal` → JSON 字符串（`"16500.0000"`）。
 * 用 `Number()` 解析会引入浮点误差（0.1 + 0.2 那类），报表的金额不能这么玩。
 *
 * # 两个已知的"预期失败"
 *
 * - `period` 没有在「会计期间」里建立 → 400 + `66010`（后端刻意不推导不存在的报告期）；
 * - `period` 格式不对 → 400 + `10001`。
 * 两者都直接显示后端返回的 `msg`，不自己编文案。
 */
import { computed, onMounted, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';

import {
  exportBalanceSheet,
  fetchBalanceSheet,
  fetchPeriodPage,
  type AccountPeriodRow,
  type BalanceSheet as BalanceSheetData,
  type ReportLine,
} from '@/api/business/erp';
import { saveBlob } from '@/utils/download';

/** 区间行：左边资产、右边负债和权益（两端行数不同，短的补空） */
interface SheetRow {
  left?: ReportLine;
  right?: ReportLine;
  /** 任一侧是合计 / 小计行 → 整行加粗 */
  total: boolean;
}

const period = ref('');
const periodOptions = ref<{ label: string; value: string }[]>([]);
const periodLoading = ref(false);
const periodHint = ref('');
const loading = ref(false);
const error = ref('');
const sheet = ref<BalanceSheetData | null>(null);

/** 负债 + 权益 拼成右侧一栏（与纸面报表的右半张一致） */
const rightLines = computed<ReportLine[]>(() => {
  if (!sheet.value) return [];
  return [...sheet.value.liabilities, ...sheet.value.equity];
});

/** 左右两栏按行号对齐 */
const rows = computed<SheetRow[]>(() => {
  const left = sheet.value?.assets ?? [];
  const right = rightLines.value;
  const total = Math.max(left.length, right.length);
  const list: SheetRow[] = [];
  for (let index = 0; index < total; index += 1) {
    const l = left[index];
    const r = right[index];
    list.push({
      left: l,
      right: r,
      total: Boolean(l?.is_total || r?.is_total),
    });
  }
  return list;
});

/**
 * 千分位 + 2 位小数；`null`（标题行 / 无上期）显示空串。
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

/** 层级缩进（1 大类不缩进、2 小类缩一级、3 明细缩两级） */
function indent(level: number): string {
  return `padding-left: ${Math.max(0, level - 1) * 16}px`;
}

/**
 * 纸面上的报表项名：减项补「减：」前缀，**只补一次**。
 *
 * V0183 种的 `ACCUM_DEPRECIATION` / `ACCUM_AMORTIZATION` 名字里已经带「减：」，
 * 无条件再拼一次就成「减：减：累计折旧」（首测时真出现了）。
 */
function itemLabel(line: ReportLine): string {
  if (line.is_deduction && !line.item_name.startsWith('减：')) {
    return `减：${line.item_name}`;
  }
  return line.item_name;
}

function itemCellClass(line: ReportLine | undefined, right = false): string[] {
  const classes = ['bs-table__item'];
  if (right) classes.push('bs-table__item--right');
  if (line?.level === 1) classes.push('bs-item--major');
  if (line?.level === 3) classes.push('bs-item--detail');
  if (line?.is_deduction) classes.push('bs-item--deduction');
  return classes;
}

/** 期间状态的中文（`DERIVED` 是报表侧独有的"未开账·按自然月推算"） */
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

/** 会计期间下拉：只列"已建立"的期间（NOT_OPENED 的不给选，选了也一定 400） */
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
    sheet.value = await fetchBalanceSheet(period.value);
  } catch (err) {
    // 后端 400/500 的 msg 已经是人话（含「差多少」「哪张凭证」），直接用
    sheet.value = null;
    error.value = (err as Error).message ?? String(err);
  } finally {
    loading.value = false;
  }
}

/** 期间切换后自动重算（卡面要求：切换期间后自动重算） */
function handlePeriodChange() {
  void load();
}

async function handleExport() {
  if (!sheet.value) return;
  try {
    const blob = await exportBalanceSheet(sheet.value.period);
    saveBlob(blob, `资产负债表_${sheet.value.period}.xlsx`);
    MessagePlugin.success('已导出');
  } catch (err) {
    MessagePlugin.error(`导出失败：${(err as Error).message ?? err}`);
  }
}

onMounted(async () => {
  await loadPeriods();
  if (period.value) {
    await load();
  }
});
</script>

<style scoped>
.bs-page {
  padding: 0;
}

.bs-card {
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

.bs-alert {
  margin-bottom: 12px;
}

.bs-head {
  text-align: center;
  margin: 8px 0 12px;
}

.bs-head__title {
  margin: 0 0 6px;
  font-size: 20px;
  font-weight: 600;
  letter-spacing: 2px;
}

.bs-head__meta {
  display: flex;
  justify-content: center;
  gap: 20px;
  flex-wrap: wrap;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.bs-body {
  overflow-x: auto;
}

.bs-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
}

.bs-table th,
.bs-table td {
  border: 1px solid var(--td-component-border);
  padding: 6px 10px;
  line-height: 1.6;
}

.bs-table thead th {
  background: var(--td-bg-color-secondarycontainer);
  font-weight: 600;
  text-align: center;
}

.bs-table__item {
  width: 26%;
  text-align: left;
}

.bs-table__item--right {
  border-left: 2px solid var(--td-component-border);
}

.bs-table__num {
  width: 11%;
  text-align: right;
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
}

.bs-table__num--prev {
  color: var(--td-text-color-placeholder);
}

/* 层级：1 大类加粗、2 小类普通、3 明细灰一点 */
.bs-item--major {
  font-weight: 600;
}

.bs-item--detail {
  color: var(--td-text-color-secondary);
}

.bs-item--deduction {
  color: var(--td-text-color-secondary);
  font-style: italic;
}

.bs-row--total td {
  font-weight: 600;
  background: var(--td-bg-color-container-hover);
}

.bs-check {
  margin-top: 14px;
  padding: 12px 14px;
  border-radius: 6px;
  font-size: 13px;
}

.bs-check--ok {
  background: var(--td-success-color-1);
  border: 1px solid var(--td-success-color-2);
  color: var(--td-success-color-7, #2ba471);
}

.bs-check--bad {
  background: var(--td-error-color-1);
  border: 1px solid var(--td-error-color-2);
  color: var(--td-error-color-7, #d54941);
}

.bs-check__line {
  font-size: 15px;
  font-weight: 600;
  margin-bottom: 6px;
}

.bs-check__icon {
  margin-right: 6px;
}

.bs-check__numbers {
  display: flex;
  gap: 18px;
  flex-wrap: wrap;
  font-variant-numeric: tabular-nums;
}

.bs-check__hints {
  margin: 8px 0 0;
  padding-left: 20px;
  line-height: 1.7;
}

.bs-check__vouchers {
  margin-top: 8px;
  line-height: 1.7;
}

.bs-check__vouchers-title {
  font-weight: 600;
}

.bs-empty {
  padding: 40px 0;
  text-align: center;
  color: var(--td-text-color-placeholder);
}
</style>

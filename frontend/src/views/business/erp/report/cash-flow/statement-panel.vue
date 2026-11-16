<!--
  由底稿生成的现金流量表（**预览**）面板（M1-18 · 后端 `cashflow_statement.rs` · 迁移 V0417）。

  这是「现金流量表」页 `报表预览` tab 的主体：读 `GET …/cash-flow/statement`（由**底稿**逐行
  汇总，不读凭证），画三栏「项目 / 本期金额 / 上期金额」，并把出表勾稽（两条等式）画清楚。

  # 四个不显眼但很关键的点

  1. **上期列可能是 `null`**：上一期间没开账 / 上期没编这一行时整格「—」，**不是 0**
     （0 与「没编」对报表是相反的结论）。判断走 `amountOrPlaceholder`。
  2. **未闭合也能打开**：`GET` 未闭合仍 200（带 `check.balanced=false` + 逐条 `problems`），
     否则会计没有入口去补底稿；**导出**才拦（400 + 66087，xlsx 一落盘就流出系统）。
  3. **上期不闭合只吼不拦**：`previous_balanced=false` 是提示，不阻塞本期出表
     （否则上期有错就永远交不出本期报表）。
  4. **`hide_zero` 是服务端参数**：隐藏「两期都没有数」的行由后端做（并回报
     `hidden_zero_lines`），前端不做过滤 —— 否则「本期 0 但上期有数」的行会被误藏。
-->
<template>
  <div class="cfs-panel">
    <div class="tool-row">
      <span class="tool-row__label">会计期间</span>
      <t-select
        v-model="period"
        class="tool-row__period"
        :options="periods"
        :loading="periodLoading"
        placeholder="选择会计期间"
        data-testid="cfs-period"
        @change="load()"
      />
      <t-button theme="primary" :loading="loading" data-testid="cfs-reload" @click="load()">查询</t-button>
      <t-checkbox v-model="hideZero" data-testid="cfs-hide-zero" @change="load()">
        隐藏本期与上期都为 0 的行
      </t-checkbox>
      <t-button
        v-privilege="[PERM_STATEMENT_EXPORT]"
        variant="outline"
        :disabled="!statement"
        data-testid="cfs-export"
        @click="handleExport()"
      >
        <template #icon><t-icon name="download" /></template>
        导出 Excel
      </t-button>
      <t-button variant="outline" :disabled="!statement" data-testid="cfs-goto-worksheet" @click="gotoWorksheet()">
        <template #icon><t-icon name="edit-1" /></template>
        去补底稿
      </t-button>
    </div>

    <t-alert v-if="error" theme="error" :message="error" class="cfs-alert" data-testid="cfs-error">
      <template #operation>
        <t-button size="small" variant="outline" @click="load()">重试</t-button>
      </template>
    </t-alert>

    <t-alert
      v-if="statement && statement.warnings.length"
      theme="warning"
      class="cfs-alert"
      data-testid="cfs-warn"
    >
      <template #message>
        <div class="cfs-warn__title">本表有 {{ statement.warnings.length }} 条口径告警（不静默处理）</div>
        <ul class="cfs-warn__list">
          <li v-for="(warning, index) in statement.warnings" :key="index" data-testid="cfs-warn-item">{{ warning }}</li>
        </ul>
      </template>
    </t-alert>

    <t-loading :loading="loading" size="small">
      <div v-if="statement" class="cfs-sheet">
        <div class="cfs-head" data-testid="cfs-head">
          <h2 class="cfs-head__title">{{ statement.report_name }}</h2>
          <div class="cfs-head__meta">
            <span data-testid="cfs-company">编制单位：{{ statement.company_name }}</span>
            <span data-testid="cfs-period-name">
              期间：{{ statement.period_name }}（{{ periodStatusText(statement.period_status) }}）
            </span>
            <span data-testid="cfs-previous-period">
              上期：{{ statement.previous_period ?? '未开账' }}
              <template v-if="statement.previous_period">
                （{{ periodStatusText(statement.previous_period_status) }}）
              </template>
            </span>
            <span>单位：{{ statement.unit }}</span>
            <span data-testid="cfs-generated-at">编制时间：{{ formatDateTime(statement.generated_at) }}</span>
          </div>
          <div class="cfs-head__note" data-testid="cfs-source-note">{{ statement.source_note }}</div>
        </div>

        <div class="cfs-table-wrap">
          <table class="cfs-table" data-testid="cfs-table">
            <thead>
              <tr>
                <th class="cfs-col-item">项目</th>
                <th class="cfs-col-num">{{ statement.current_label }}</th>
                <th class="cfs-col-num">{{ statement.previous_label }}</th>
              </tr>
            </thead>
            <tbody>
              <template v-for="section in statement.sections" :key="section.activity">
                <tr class="cfs-row--activity" :data-testid="`cfs-section-${section.activity}`">
                  <td class="cfs-col-item">{{ section.activity_name }}</td>
                  <td class="cfs-col-num" />
                  <td class="cfs-col-num" />
                </tr>

                <tr
                  v-for="line in section.inflow"
                  :key="`in-${line.item_code}`"
                  class="cfs-row--line"
                  :data-testid="`cfs-line-${line.item_code}`"
                >
                  <td class="cfs-col-item">
                    <span class="cfs-indent">{{ line.item_name }}</span>
                    <t-tag v-if="!line.item_active" size="small" variant="light" class="cfs-tag">已停用</t-tag>
                    <t-tag v-if="!line.current_entered" size="small" variant="light" theme="warning" class="cfs-tag">
                      本期未录
                    </t-tag>
                  </td>
                  <td class="cfs-col-num" :data-testid="`cfs-current-${line.item_code}`">{{ formatAmount(line.current) }}</td>
                  <td class="cfs-col-num" :data-testid="`cfs-previous-${line.item_code}`">{{ amountOrPlaceholder(line.previous) }}</td>
                </tr>
                <tr class="cfs-row--subtotal" :data-testid="`cfs-inflow-total-${section.activity}`">
                  <td class="cfs-col-item"><span class="cfs-indent">现金流入小计</span></td>
                  <td class="cfs-col-num">{{ formatAmount(section.inflow_total.current) }}</td>
                  <td class="cfs-col-num">{{ amountOrPlaceholder(section.inflow_total.previous) }}</td>
                </tr>

                <tr
                  v-for="line in section.outflow"
                  :key="`out-${line.item_code}`"
                  class="cfs-row--line"
                  :data-testid="`cfs-line-${line.item_code}`"
                >
                  <td class="cfs-col-item">
                    <span class="cfs-indent">{{ line.item_name }}</span>
                    <t-tag v-if="!line.item_active" size="small" variant="light" class="cfs-tag">已停用</t-tag>
                    <t-tag v-if="!line.current_entered" size="small" variant="light" theme="warning" class="cfs-tag">
                      本期未录
                    </t-tag>
                  </td>
                  <td class="cfs-col-num" :data-testid="`cfs-current-${line.item_code}`">{{ formatAmount(line.current) }}</td>
                  <td class="cfs-col-num" :data-testid="`cfs-previous-${line.item_code}`">{{ amountOrPlaceholder(line.previous) }}</td>
                </tr>
                <tr class="cfs-row--subtotal" :data-testid="`cfs-outflow-total-${section.activity}`">
                  <td class="cfs-col-item"><span class="cfs-indent">现金流出小计</span></td>
                  <td class="cfs-col-num">{{ formatAmount(section.outflow_total.current) }}</td>
                  <td class="cfs-col-num">{{ amountOrPlaceholder(section.outflow_total.previous) }}</td>
                </tr>

                <tr class="cfs-row--net" :data-testid="`cfs-net-${section.activity}`">
                  <td class="cfs-col-item">{{ ACTIVITY_LABELS[section.activity] ?? section.activity_name }}产生的现金流量净额</td>
                  <td class="cfs-col-num" :data-testid="`cfs-net-current-${section.activity}`">{{ formatAmount(section.net.current) }}</td>
                  <td class="cfs-col-num" :data-testid="`cfs-net-previous-${section.activity}`">{{ amountOrPlaceholder(section.net.previous) }}</td>
                </tr>
              </template>

              <tr class="cfs-row--total" data-testid="cfs-net-increase">
                <td class="cfs-col-item">现金及现金等价物净增加额</td>
                <td class="cfs-col-num" data-testid="cfs-net-increase-current">{{ formatAmount(statement.net_increase.current) }}</td>
                <td class="cfs-col-num" data-testid="cfs-net-increase-previous">{{ amountOrPlaceholder(statement.net_increase.previous) }}</td>
              </tr>
              <tr class="cfs-row--total" data-testid="cfs-opening">
                <td class="cfs-col-item">加：{{ statement.opening.item_name }}</td>
                <td class="cfs-col-num" data-testid="cfs-opening-current">{{ amountOrPlaceholder(statement.opening.current) }}</td>
                <td class="cfs-col-num" data-testid="cfs-opening-previous">{{ amountOrPlaceholder(statement.opening.previous) }}</td>
              </tr>
              <tr class="cfs-row--total" data-testid="cfs-closing">
                <td class="cfs-col-item">{{ statement.closing.item_name }}</td>
                <td class="cfs-col-num" data-testid="cfs-closing-current">{{ amountOrPlaceholder(statement.closing.current) }}</td>
                <td class="cfs-col-num" data-testid="cfs-closing-previous">{{ amountOrPlaceholder(statement.closing.previous) }}</td>
              </tr>
            </tbody>
          </table>
        </div>

        <div class="cfs-foot-meta" data-testid="cfs-line-count">
          列示 {{ statement.line_count }} 行<template v-if="statement.hidden_zero_lines > 0">（按「隐藏零行」隐去 {{ statement.hidden_zero_lines }} 行）</template>
        </div>

        <!-- ============ 出表勾稽（两条等式）============ -->
        <div :class="['cfs-check', `is-check--${tone}`]" data-testid="cfs-check">
          <div class="cfs-check__line">
            <span class="cfs-check__icon">{{ tone === 'ok' ? '✅' : '⚠️' }}</span>
            <span data-testid="cfs-check-message">{{ statement.check.message }}</span>
          </div>
          <div class="cfs-check__numbers">
            <span>
              本期是否闭合
              <b :data-testid="`cfs-check-balanced`">{{ statement.check.balanced ? '闭合' : '不闭合' }}</b>
            </span>
            <span>
              报表加减关系（净增加额 + 期初 = 期末）
              <b data-testid="cfs-check-arithmetic">{{ statement.check.arithmetic_ok ? '成立' : '不成立' }}</b>
            </span>
            <span>差额 <b data-testid="cfs-check-diff">{{ diffText(statement.check.diff) }}</b></span>
            <span data-testid="cfs-check-previous">
              上期闭合
              <b>
                {{ statement.check.previous_balanced === null ? '无上期底稿' : statement.check.previous_balanced ? '是' : '否（只提示，不拦本期）' }}
              </b>
            </span>
          </div>
          <ul v-if="statement.check.current.problems.length" class="cfs-check__problems" data-testid="cfs-problems">
            <li v-for="problem in statement.check.current.problems" :key="problem.code">
              <b>{{ problemLabel(problem.code) }}</b>：{{ problemText(problem) }}
            </li>
          </ul>
          <div v-if="statement.check.previous && !statement.check.previous.balanced" class="cfs-check__previous" data-testid="cfs-previous-problems">
            上期底稿不闭合：{{ statement.check.previous.message }}
          </div>
          <div v-if="!statement.check.balanced" class="cfs-check__hint">
            本表未闭合：页面照常可看，但「导出 Excel」会被后端拦下（66087）。请到「现金流底稿」tab 补齐后重试。
          </div>
        </div>
      </div>

      <div v-else-if="!loading && !error" class="cfs-empty" data-testid="cfs-empty">
        选择会计期间后点「查询」预览由底稿生成的现金流量表
      </div>
    </t-loading>
  </div>
</template>

<script setup lang="ts">
/**
 * 由底稿生成的现金流量表（预览）。
 *
 * 页面不做任何金额计算：三栏金额 / 小计 / 净额 / 净增加额 / 期初 / 期末 / 差额全部来自后端；
 * 前端只做千分位格式化与「`null` 显示占位符」。
 */
import { computed, onMounted, ref, watch } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';

import { fetchPeriodPage, type AccountPeriodRow } from '@/api/business/erp';
import {
  exportCashflowStatement,
  fetchCashflowStatement,
  type CashStatement,
} from '@/api/business/erp/cashflow-worksheet';
import { saveBlob } from '@/utils/download';
import {
  ACTIVITY_LABELS,
  PERM_STATEMENT_EXPORT,
  amountOrPlaceholder,
  diffText,
  exportBlockedReason,
  formatAmount,
  formatDateTime,
  periodOptions,
  periodStatusText,
  problemLabel,
  problemText,
  statementCheckTone,
} from '@/views/business/erp/report/cash-flow/worksheet-logic';

const props = defineProps<{ period?: string }>();
const emit = defineEmits<{ (event: 'goto-worksheet', period: string): void }>();

const period = ref(props.period ?? '');
const periods = ref<{ label: string; value: string }[]>([]);
const periodLoading = ref(false);
const loading = ref(false);
const exporting = ref(false);
const hideZero = ref(false);
const error = ref('');
const statement = ref<CashStatement | null>(null);

const tone = computed(() => (statement.value ? statementCheckTone(statement.value.check) : 'bad'));

async function loadPeriods() {
  periodLoading.value = true;
  try {
    const data = await fetchPeriodPage({ page: 1, page_size: 100 });
    periods.value = periodOptions(data.list as AccountPeriodRow[]);
    if (!period.value && periods.value.length) {
      period.value = periods.value[0].value;
    }
  } catch (err) {
    error.value = `期间列表加载失败：${(err as Error).message ?? err}`;
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
    statement.value = await fetchCashflowStatement(period.value, hideZero.value);
  } catch (err) {
    statement.value = null;
    error.value = (err as Error).message ?? String(err);
  } finally {
    loading.value = false;
  }
}

async function handleExport() {
  const data = statement.value;
  if (!data) return;
  // 前端先按已加载的勾稽结论拦一次（未闭合时后端回 400+66087，但 blob 失败路径读不到 msg）
  const blocked = exportBlockedReason(data.check, data.check.current.problems);
  if (blocked) {
    MessagePlugin.error(`底稿勾稽未闭合，导出被拦：${blocked}。请回「现金流底稿」补齐后重试`);
    return;
  }
  exporting.value = true;
  try {
    const blob = await exportCashflowStatement(data.period);
    saveBlob(blob, `现金流量表_底稿_${data.period}.xlsx`);
    MessagePlugin.success('已导出');
  } catch (err) {
    // 兜底：加载后数据被别人改过（或导出口径另有问题），仍把后端原文尽量展示出来
    MessagePlugin.error(`导出失败：${(err as Error).message ?? err}`);
  } finally {
    exporting.value = false;
  }
}

function gotoWorksheet() {
  if (!statement.value) return;
  emit('goto-worksheet', statement.value.period);
}

watch(
  () => props.period,
  (next) => {
    if (next && next !== period.value) {
      period.value = next;
      void load();
    }
  },
);

onMounted(async () => {
  await loadPeriods();
  if (period.value) {
    await load();
  }
});

defineExpose({ load });
</script>

<style scoped>
.cfs-alert {
  margin-bottom: 12px;
}

.cfs-warn__title {
  font-weight: 600;
  margin-bottom: 4px;
}

.cfs-warn__list {
  margin: 0;
  padding-left: 18px;
  line-height: 1.7;
}

.cfs-head {
  text-align: center;
  margin: 8px 0 12px;
}

.cfs-head__title {
  margin: 0 0 6px;
  font-size: 20px;
  font-weight: 600;
  letter-spacing: 2px;
}

.cfs-head__meta {
  display: flex;
  justify-content: center;
  gap: 20px;
  flex-wrap: wrap;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.cfs-head__note {
  margin-top: 6px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
  line-height: 1.6;
}

.cfs-table-wrap {
  overflow-x: auto;
}

.cfs-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
}

.cfs-table th,
.cfs-table td {
  border: 1px solid var(--td-component-border);
  padding: 6px 10px;
  line-height: 1.6;
}

.cfs-table thead th {
  background: var(--td-bg-color-secondarycontainer);
  font-weight: 600;
  text-align: center;
}

.cfs-col-item {
  width: 56%;
  text-align: left;
}

.cfs-col-num {
  width: 22%;
  text-align: right;
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
}

.cfs-indent {
  padding-left: 16px;
}

.cfs-tag {
  margin-left: 6px;
}

.cfs-row--activity td {
  font-weight: 600;
  background: var(--td-bg-color-secondarycontainer);
}

.cfs-row--subtotal td {
  font-weight: 600;
}

.cfs-row--net td {
  font-weight: 600;
  background: var(--td-bg-color-container-hover);
}

.cfs-row--total td {
  font-weight: 600;
  background: var(--td-bg-color-container-active);
}

.cfs-foot-meta {
  margin-top: 6px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.cfs-check {
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
  background: var(--td-warning-color-1);
  border: 1px solid var(--td-warning-color-2);
  color: var(--td-warning-color-7, #b25e00);
}

.cfs-check__line {
  font-size: 15px;
  font-weight: 600;
  margin-bottom: 6px;
}

.cfs-check__icon {
  margin-right: 6px;
}

.cfs-check__numbers {
  display: flex;
  gap: 18px;
  flex-wrap: wrap;
  font-variant-numeric: tabular-nums;
}

.cfs-check__problems {
  margin: 8px 0 0;
  padding-left: 20px;
  line-height: 1.7;
}

.cfs-check__previous,
.cfs-check__hint {
  margin-top: 8px;
  line-height: 1.7;
}

.cfs-empty {
  padding: 40px 0;
  text-align: center;
  color: var(--td-text-color-placeholder);
}
</style>

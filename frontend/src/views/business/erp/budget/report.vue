<!--
  预算执行报表页（FIN-1-FE / ARCH-6 §3.5）。

  口径全部来自后端 `ReportRes`（前端不重算）：
  - **年度**：`occupied`（占用中，采购单已提交未入库）/ `consumed`（已执行）/
    `available`（可为负 = 超支）/ `over` —— 整年的账；
  - **月度**：`month_over` / `month_over_amount` / `month_available`（+ `month_period` 定位到哪一月）
    —— 本年度**最紧张的那个月**，与提交侧 `budget_guard::MonthGate` 同源（E2E06-FIX-2）；
  - `exec_ratio` 是后端算好的百分比字符串，`effective` 是后端按「该行年度期末」判的（不是「今天」）。

  合计行由 `summarizeReport` 用**整数分**累加（后端逐槽返回、没有合计行；
  12 个槽各带 4 位小数时浮点累加会留下 0.0000000001 的尾巴）。

  接口 `GET /api/business/erp/budget/report` **不分页**（返回 `Vec<ReportRes>`）。
-->
<template>
  <div class="report-page">
    <!-- ==================== ① 汇总卡片 ==================== -->
    <div class="stat-row" data-testid="budget-report-stats">
      <div class="stat-card">
        <span class="stat-card__label">额度合计</span>
        <span class="stat-card__value">{{ formatCents(summary.totalCents) }}</span>
        <span class="stat-card__sub">{{ summary.count }} 个「部门 × 科目」槽</span>
      </div>
      <div class="stat-card">
        <span class="stat-card__label">占用中</span>
        <span class="stat-card__value stat-card__value--primary">{{ formatCents(summary.occupiedCents) }}</span>
        <span class="stat-card__sub">采购单已提交、尚未入库</span>
      </div>
      <div class="stat-card">
        <span class="stat-card__label">已执行</span>
        <span class="stat-card__value">{{ formatCents(summary.consumedCents) }}</span>
        <span class="stat-card__sub">已入账的实际发生额</span>
      </div>
      <div class="stat-card">
        <span class="stat-card__label">已用合计</span>
        <span class="stat-card__value">{{ formatCents(summary.usedCents) }}</span>
        <span class="stat-card__sub">占用中 + 已执行</span>
      </div>
      <div class="stat-card">
        <span class="stat-card__label">可用合计</span>
        <span class="stat-card__value" :class="{ 'stat-card__value--bad': summary.availableCents < 0 }">
          {{ formatCents(summary.availableCents) }}
        </span>
        <span class="stat-card__sub">负数 = 超支</span>
      </div>
      <div class="stat-card">
        <span class="stat-card__label">全公司执行率</span>
        <span class="stat-card__value">{{ summary.ratio.toFixed(2) }}%</span>
        <span class="stat-card__sub" :class="{ 'stat-card__sub--bad': summary.overCount > 0 || summary.monthOverCount > 0 }">
          年度超支 {{ summary.overCount }} 个<template v-if="summary.monthOverCount"> · 本月超 {{ summary.monthOverCount }} 个</template>
        </span>
      </div>
    </div>

    <!-- ==================== ② 筛选 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-select
          v-model="yearFilterValue"
          class="tool-row__year"
          :options="yearOptions"
          placeholder="全部年度"
          clearable
          data-testid="budget-report-filter-year"
          @change="loadRows"
        />
        <t-select
          v-model="filter.dept_id"
          class="tool-row__dept"
          :options="deptOptions"
          placeholder="全部部门"
          clearable
          filterable
          data-testid="budget-report-filter-dept"
          @change="loadRows"
        />
        <t-input
          v-model="filter.keyword"
          class="tool-row__keyword"
          placeholder="单号 / 科目 / 部门（模糊）"
          clearable
          data-testid="budget-report-filter-keyword"
          @enter="loadRows"
        />
        <t-button theme="primary" data-testid="budget-report-btn-search" @click="loadRows">查询</t-button>
        <t-button variant="outline" data-testid="budget-report-btn-reset" @click="handleReset">重置</t-button>
        <span class="tool-row__spacer" />
        <span class="tool-row__hint">
          只有「已生效（ACTIVE）」的预算会进报表；草稿 / 审批中的版本不参与占用控制
        </span>
      </div>
    </t-card>

    <!-- ==================== ③ 执行明细 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">预算执行</span>
          <span class="panel-head__meta" data-testid="budget-report-meta">共 {{ rows.length }} 个槽</span>
        </div>
        <div class="panel-head__actions">
          <t-button size="small" variant="outline" data-testid="budget-report-toggle-all" @click="toggleAll">
            {{ allExpanded ? '全部收起' : '全部分月' }}
          </t-button>
          <t-button size="small" variant="outline" @click="loadRows">刷新</t-button>
        </div>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="budget-report-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadRows">重试</t-button>
        </template>
      </t-alert>

      <t-table
        v-if="rows.length || loading"
        row-key="budget_id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :expand-icon="true"
        :expanded-row-keys="expandedKeys"
        hover
        data-testid="budget-report-table"
        @expand-change="handleExpandChange"
      >
        <template #dept_name="{ row }">
          <div class="name-cell">
            <span class="name-cell__main">{{ row.dept_name }}</span>
            <span class="name-cell__sub">{{ row.fiscal_year }} 年度</span>
          </div>
        </template>
        <template #account="{ row }">
          <div class="name-cell">
            <span class="name-cell__main">{{ row.account_name }}</span>
            <span class="name-cell__sub">{{ row.account_code }}</span>
          </div>
        </template>
        <template #total_amount="{ row }">
          <span class="money">{{ formatMoney(row.total_amount) }}</span>
        </template>
        <template #occupied="{ row }">
          <span class="money">{{ formatMoney(row.occupied) }}</span>
        </template>
        <template #consumed="{ row }">
          <span class="money">{{ formatMoney(row.consumed) }}</span>
        </template>
        <template #used="{ row }">
          <span class="money money--strong">{{ formatMoney(row.used) }}</span>
        </template>
        <template #available="{ row }">
          <span class="money" :class="{ 'money--bad': isNegative(row.available) }">
            {{ formatMoney(row.available) }}
          </span>
        </template>
        <template #exec_ratio="{ row }">
          <div class="ratio-cell">
            <t-progress
              :percentage="Math.min(100, Math.max(0, toNumber(row.exec_ratio)))"
              :label="false"
              :theme="'line'"
              :status="row.over || row.month_over ? 'error' : undefined"
              size="small"
            />
            <span class="ratio-cell__text" :class="{ 'ratio-cell__text--bad': row.over || row.month_over }">
              {{ ratioText(row.exec_ratio) }}
            </span>
          </div>
        </template>
        <!-- 「本月」列：后端给的是本年度**最紧张的那个月**（超支优先，其次可用最小） -->
        <template #month="{ row }">
          <div v-if="row.month_period" class="month-cell" :data-testid="`budget-report-month-${row.budget_id}`">
            <span class="money" :class="{ 'money--bad': row.month_over }">
              {{ formatMoney(row.month_available) }}
            </span>
            <span class="name-cell__sub">
              {{ row.month_period }}
              <template v-if="row.month_over">· 超 {{ formatMoney(row.month_over_amount) }}</template>
            </span>
          </div>
          <span v-else class="muted">—</span>
        </template>
        <template #over="{ row }">
          <t-tag
            size="small"
            variant="light"
            :theme="badge(row).theme"
            :title="badge(row).hint"
            :data-testid="`budget-report-badge-${row.budget_id}`"
          >
            {{ badge(row).text }}
          </t-tag>
        </template>
        <template #expandedRow="{ row }">
          <div class="month-expand" :data-testid="`budget-report-months-${row.budget_id}`">
            <div class="month-expand__title">分月执行（共 12 个月，可用为负数 = 该月超支）</div>
            <t-table
              row-key="period_code"
              size="small"
              :data="reportMonths(row)"
              :columns="monthColumns"
            >
              <template #amount="{ row: month }">
                <span class="money">{{ formatMoney(month.amount) }}</span>
              </template>
              <template #used="{ row: month }">
                <span class="money">{{ formatMoney(month.used) }}</span>
              </template>
              <template #available="{ row: month }">
                <span class="money" :class="{ 'money--bad': month.over }">{{ formatMoney(month.available) }}</span>
              </template>
            </t-table>
          </div>
        </template>
      </t-table>

      <t-empty
        v-if="!loading && !rows.length && !loadError"
        class="empty"
        :title="reportEmptyText(filter.fiscal_year)"
        description="预算要先「编制 → 提交 → 审批通过（ACTIVE）」才会出现在执行报表里"
        data-testid="budget-report-empty"
      />
    </t-card>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';

import { fetchBudgetReport, type BudgetReportRow } from '@/api/business/erp/budget';
import { getDeptTree, type DeptNode } from '@/api/system/dept-api';
import { formatCents, formatMoney, toNumber } from '@/views/business/erp/shared/money';
import {
  fiscalYearOptions,
  ratioText,
  reportBadge,
  reportEmptyText,
  reportMonths,
  summarizeReport,
  type ReportBadge,
} from './budget-logic';

const rows = ref<BudgetReportRow[]>([]);
const loading = ref(false);
const loadError = ref('');
const expandedKeys = ref<Array<string | number>>([]);

const filter = reactive<{ fiscal_year: number | null; dept_id: string; keyword: string }>({
  fiscal_year: new Date().getFullYear(),
  dept_id: '',
  keyword: '',
});

/** `t-select` 不接受 `null`：`null`（全部年度）与 `undefined` 互转 */
const yearFilterValue = computed<number | undefined>({
  get: () => filter.fiscal_year ?? undefined,
  set: (value) => {
    filter.fiscal_year = typeof value === 'number' ? value : null;
  },
});

const yearOptions = fiscalYearOptions().map((year) => ({ label: `${year} 年度`, value: year }));

const depts = ref<DeptNode[]>([]);
const deptOptions = computed(() => {
  const out: Array<{ label: string; value: string }> = [];
  const walk = (nodes: DeptNode[], depth: number): void => {
    for (const node of nodes) {
      out.push({ label: `${'　'.repeat(depth)}${node.dept_name}`, value: node.id });
      if (node.children?.length) {
        walk(node.children, depth + 1);
      }
    }
  };
  walk(depts.value, 0);
  return out;
});

const summary = computed(() => summarizeReport(rows.value));

const allExpanded = computed(
  () => rows.value.length > 0 && expandedKeys.value.length === rows.value.length,
);

const columns: PrimaryTableCol[] = [
  { colKey: 'dept_name', title: '部门 / 年度', width: 132, ellipsis: true },
  { colKey: 'account', title: '预算科目', minWidth: 150, ellipsis: true },
  { colKey: 'total_amount', title: '年度额度', width: 110, align: 'right' },
  { colKey: 'occupied', title: '占用中', width: 102, align: 'right' },
  { colKey: 'consumed', title: '已执行', width: 102, align: 'right' },
  { colKey: 'used', title: '已用合计', width: 110, align: 'right' },
  { colKey: 'available', title: '可用', width: 110, align: 'right' },
  { colKey: 'month', title: '本月', width: 132, align: 'right' },
  { colKey: 'exec_ratio', title: '执行率', width: 128 },
  { colKey: 'over', title: '预算', width: 104 },
];

const monthColumns: PrimaryTableCol[] = [
  { colKey: 'period_code', title: '期间', width: 96 },
  { colKey: 'amount', title: '额度', width: 130, align: 'right' },
  { colKey: 'used', title: '已用', width: 130, align: 'right' },
  { colKey: 'available', title: '可用', width: 130, align: 'right' },
];

/** 可用为负 = 超支（后端也算好了 `over`，这里只用于给数字上色） */
function isNegative(value: string | number): boolean {
  return toNumber(value) < 0;
}

/**
 * 「预算」列的徽标（月度优先）。
 *
 * 口径全部来自后端（`month_over` / `over` / `effective`），这里只决定**怎么显示**：
 * 月度闸才是提交侧实际控制的闸门，所以月度超支优先于年度超支显示。
 */
function badge(row: BudgetReportRow): ReportBadge {
  return reportBadge(row);
}

function handleExpandChange(keys: Array<string | number>): void {
  expandedKeys.value = keys;
}

function toggleAll(): void {
  expandedKeys.value = allExpanded.value ? [] : rows.value.map((row) => row.budget_id);
}

async function loadRows(): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    rows.value = await fetchBudgetReport({
      fiscal_year: filter.fiscal_year ?? undefined,
      dept_id: filter.dept_id.trim() || undefined,
      keyword: filter.keyword.trim() || undefined,
    });
    // 收起已不存在的行（筛选后行集变了）
    const alive = new Set(rows.value.map((row) => row.budget_id));
    expandedKeys.value = expandedKeys.value.filter((key) => alive.has(String(key)));
  } catch (error) {
    rows.value = [];
    loadError.value = `预算执行报表加载失败：${errorText(error)}`;
  } finally {
    loading.value = false;
  }
}

function handleReset(): void {
  filter.fiscal_year = new Date().getFullYear();
  filter.dept_id = '';
  filter.keyword = '';
  expandedKeys.value = [];
  void loadRows();
}

async function loadDepts(): Promise<void> {
  try {
    depts.value = await getDeptTree();
  } catch (error) {
    MessagePlugin.warning(`部门树加载失败：${errorText(error)}`);
  }
}

/** 后端 `ApiError` 出参 `{ code, message }` —— 原样透出，不吞信息 */
function errorText(error: unknown): string {
  if (error && typeof error === 'object') {
    const like = error as { message?: string; code?: number };
    const message = (like.message ?? '').trim();
    if (message) {
      return like.code ? `${message}（${like.code}）` : message;
    }
  }
  if (typeof error === 'string' && error.trim()) {
    return error.trim();
  }
  return '请求失败，请稍后重试';
}

onMounted(() => {
  void loadDepts();
  void loadRows();
});
</script>

<style scoped lang="less">
.report-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.stat-row {
  display: grid;
  grid-template-columns: repeat(6, minmax(0, 1fr));
  gap: 12px;
}

.stat-card {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 14px 16px;
  border-radius: 8px;
  border: 1px solid var(--td-component-stroke);
  background: var(--td-bg-color-container);

  &__label {
    color: var(--td-text-color-secondary);
    font-size: 12px;
  }

  &__value {
    font-size: 20px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    line-height: 1.2;

    &--primary {
      color: var(--td-brand-color);
    }

    &--bad {
      color: var(--td-error-color);
    }
  }

  &__sub {
    color: var(--td-text-color-placeholder);
    font-size: 12px;

    &--bad {
      color: var(--td-error-color);
    }
  }
}

.tool-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;

  &__spacer {
    flex: 1;
  }

  &__year {
    width: 112px;
  }

  &__dept {
    width: 160px;
  }

  &__keyword {
    width: 200px;
  }

  &__hint {
    color: var(--td-text-color-placeholder);
    font-size: 12px;
  }
}

.list-alert {
  margin-bottom: 12px;
}

.panel-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 12px;

  &__main {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }

  &__title {
    font-size: 16px;
    font-weight: 600;
  }

  &__meta {
    color: var(--td-text-color-placeholder);
    font-size: 12px;
  }

  &__actions {
    display: flex;
    gap: 8px;
  }
}

.name-cell {
  display: flex;
  flex-direction: column;
  line-height: 1.35;

  &__main {
    font-weight: 500;
  }

  &__sub {
    color: var(--td-text-color-placeholder);
    font-size: 12px;
  }
}

.money {
  font-variant-numeric: tabular-nums;

  &--strong {
    font-weight: 600;
  }

  &--bad {
    color: var(--td-error-color);
  }
}

.muted {
  color: var(--td-text-color-placeholder);
}

.month-cell {
  display: flex;
  flex-direction: column;
  line-height: 1.35;
  align-items: flex-end;
}

.ratio-cell {
  display: flex;
  align-items: center;
  gap: 8px;

  &__text {
    font-size: 12px;
    font-variant-numeric: tabular-nums;
    color: var(--td-text-color-secondary);
    white-space: nowrap;

    &--bad {
      color: var(--td-error-color);
      font-weight: 600;
    }
  }

  :deep(.t-progress) {
    flex: 1;
    min-width: 48px;
  }
}

.month-expand {
  padding: 8px 12px 12px 40px;
  background: var(--td-bg-color-page);
  border-radius: 6px;

  &__title {
    font-size: 12px;
    color: var(--td-text-color-secondary);
    margin-bottom: 8px;
  }
}

.empty {
  margin-top: 16px;
}
</style>

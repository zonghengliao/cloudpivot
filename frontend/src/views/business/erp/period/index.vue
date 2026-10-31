<template>
  <div class="period-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button variant="outline" data-testid="btn-period-refresh" @click="loadPeriods()">
          <template #icon><t-icon name="refresh" /></template>
          刷新
        </t-button>

        <span class="tool-row__spacer" />

        <t-select
          v-model="yearModel"
          class="tool-row__year"
          :options="yearOptions"
          placeholder="全部年份"
          clearable
          data-testid="period-filter-year"
          @change="handleSearch"
        />
        <t-select
          v-model="statusModel"
          class="tool-row__status"
          :options="statusOptions"
          placeholder="全部状态"
          clearable
          data-testid="period-filter-status"
          @change="handleSearch"
        />
        <t-button theme="primary" data-testid="btn-period-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-period-reset" @click="handleReset">重置</t-button>
      </div>
    </t-card>

    <!-- ==================== ② 期间列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">会计期间</span>
          <span class="panel-head__meta" data-testid="period-meta">共 {{ total }} 个期间</span>
        </div>
        <!-- ★ 本页只读：开账 / 结账是财务主管的动作，误点会让整个月的账进不去 -->
        <span class="panel-head__tip" data-testid="period-readonly-tip">
          只读：开账 / 结账由财务主管操作
        </span>
      </div>

      <t-alert
        v-if="apiMissing"
        theme="info"
        message="会计期间接口尚未上线"
        class="list-alert"
        data-testid="period-api-missing"
      >
        <template #description>
          <span class="mono">GET /api/business/erp/period/page</span> 返回 404。接口上线后本页无需改动。
        </template>
      </t-alert>

      <t-alert
        v-else-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="period-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadPeriods()">重试</t-button>
        </template>
      </t-alert>

      <t-table
        v-if="rows.length > 0 || loading"
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        :row-class-name="rowClassName"
        hover
        data-testid="period-table"
        @page-change="handlePageChange"
      >
        <template #period_code="{ row }">
          <span class="mono period-code" :data-testid="`period-code-${row.period_code}`">
            {{ row.period_code }}
          </span>
          <t-tag
            v-if="isCurrentPeriodRow(row)"
            size="small"
            variant="light"
            theme="primary"
            class="current-tag"
            data-testid="period-current-mark"
          >
            当月
          </t-tag>
        </template>

        <template #range="{ row }">
          <span class="muted" :data-testid="`period-range-${row.period_code}`">{{ periodRangeText(row) }}</span>
          <span class="muted days">（{{ periodDays(row) }} 天）</span>
        </template>

        <template #status="{ row }">
          <t-tag
            size="small"
            variant="light"
            :theme="periodStatusTheme(row.status)"
            :data-testid="`period-status-${row.period_code}`"
          >
            {{ periodStatusLabel(row.status) }}
          </t-tag>
        </template>

        <template #closed_at="{ row }">
          <span class="muted" :data-testid="`period-closed-${row.period_code}`">{{ closedAtText(row) }}</span>
        </template>
      </t-table>

      <div v-else class="panel-empty" data-testid="period-empty">
        <t-icon name="calendar" class="panel-empty__icon" />
        <p class="panel-empty__title">{{ filtered ? '没有匹配的会计期间' : '暂无会计期间' }}</p>
        <p class="panel-empty__desc">
          {{ filtered ? '换个年份或状态再试' : '期间由财务主管按月开账后才有数据' }}
        </p>
      </div>
    </t-card>
  </div>
</template>

<script setup lang="ts">
/**
 * 会计期间页（P4-C）——**只读列表**。
 *
 * # 字段来源（硬红线 1：以库列 / DTO 为准）
 *
 * `psql \d biz_accounting_period`：`period_year / period_month / period_code(VARCHAR 7) /
 * start_date / end_date / status(NOT_OPENED|OPEN|CLOSED) / closed_at / closed_by`；
 * 出参 `dto.rs::PeriodRes` 同名 + `id / remark / created_at / updated_at`。
 *
 * # 为什么服务端分页（与科目页相反）
 *
 * 期间是「一月一条」的流水型数据，**没有父子关系**，分页不会破坏结构；
 * 一年 12 条、十年 120 条，`t-table` 自带分页 + 后端 `period/page` 就是为这个设计的。
 *
 * # 只读（硬红线 2）
 *
 * 没有「开账 / 结账 / 改日期」按钮：结账是财务封账动作（结错一个月要反结账 + 调凭证），
 * 本批不给入口。页头一行字说明责任在财务主管。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import type { PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import { fetchPeriodPage, type AccountPeriodRow } from '@/api/business/erp';
import {
  PERIOD_STATUS_OPTIONS,
  buildPeriodQuery,
  closedAtText,
  emptyPeriodFilter,
  isCurrentPeriod,
  isPeriodFiltered,
  periodDays,
  periodRangeText,
  periodStatusLabel,
  periodStatusTheme,
  periodYearOptions,
  type PeriodFilterState,
} from './period-logic';

/** 每页条数（一年 12 条 → 一页看不完也不至于翻页太频繁，取 20） */
const PAGE_SIZE = 20;

/** 表格列 */
const columns: PrimaryTableCol[] = [
  { colKey: 'period_code', title: '期间', width: 160 },
  { colKey: 'range', title: '起止日期', minWidth: 260 },
  { colKey: 'status', title: '状态', width: 110 },
  { colKey: 'closed_at', title: '结账时间', width: 200 },
];

const statusOptions = PERIOD_STATUS_OPTIONS.map((item) => ({ ...item }));

const filter = reactive<PeriodFilterState>(emptyPeriodFilter());

/**
 * `t-select` 的 `v-model` 不收 `null`，而纯逻辑里「全部」用 `null` / `''`。
 * 两个双向 computed 做桥：清空 → `null` / `''`，选中 → 原值。
 */
const yearModel = computed<number | string | undefined>({
  get: () => (filter.period_year === null ? undefined : filter.period_year),
  set: (value) => {
    filter.period_year = typeof value === 'number' ? value : null;
  },
});

/** 状态下拉的桥（同上：`''` = 全部） */
const statusModel = computed<string | undefined>({
  get: () => filter.status || undefined,
  set: (value) => {
    filter.status = (typeof value === 'string' ? value : '') as PeriodFilterState['status'];
  },
});

const rows = ref<AccountPeriodRow[]>([]);
const total = ref(0);
const current = ref(1);
const loading = ref(false);
const loadError = ref('');
/** 接口未注册（404）：不报红，改成信息条 + 空态 */
const apiMissing = ref(false);

/** 年份下拉：数据里出现过的年份 + 当前年，倒序 */
const yearOptions = computed(() =>
  periodYearOptions(rows.value).map((year) => ({ label: `${year} 年`, value: year })),
);

const filtered = computed(() => isPeriodFiltered(filter));

const pagination = computed(() => ({
  current: current.value,
  pageSize: PAGE_SIZE,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

// ==================== 行为 ====================

/** 错误 → 文案 */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/** 未注册路由 = HTTP 404；业务 404 = `10002` */
function isApiMissing(error: unknown): boolean {
  if (!(error instanceof ApiError)) {
    return false;
  }
  if (error.code === 404) {
    return true;
  }
  return error.message.includes('404') || error.message.includes('请求的资源不存在');
}

/** 当前会计期间（页面高亮 + 「当月」标记） */
function isCurrentPeriodRow(row: AccountPeriodRow): boolean {
  return isCurrentPeriod(row);
}

/** 当前期间行加浅底（让「这个月」在长列表里一眼可见） */
function rowClassName({ row }: { row: AccountPeriodRow }): string {
  return isCurrentPeriodRow(row) ? 'period-row--current' : '';
}

/** 拉列表 */
async function loadPeriods(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchPeriodPage(buildPeriodQuery(filter, page, PAGE_SIZE));
    apiMissing.value = false;
    rows.value = data.list ?? [];
    total.value = data.total ?? rows.value.length;
    const rawPage = data.page ?? page;
    const maxPage = Math.max(1, Math.ceil(total.value / PAGE_SIZE));
    current.value = Math.min(Math.max(1, rawPage), maxPage);
  } catch (error) {
    rows.value = [];
    total.value = 0;
    apiMissing.value = isApiMissing(error);
    loadError.value = apiMissing.value ? '' : messageOf(error);
  } finally {
    loading.value = false;
  }
}

/** 查询：条件变了回第 1 页（否则在第 3 页筛选出 1 条会看到空表） */
function handleSearch(): void {
  void loadPeriods(1);
}

/** 重置 */
function handleReset(): void {
  Object.assign(filter, emptyPeriodFilter());
  void loadPeriods(1);
}

/** 翻页 */
function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  void loadPeriods(pageInfo.current);
}

onMounted(() => {
  void loadPeriods(1);
});
</script>

<style scoped lang="less">
.period-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

/* ==================== 工具栏 ==================== */

.tool-card {
  flex: 0 0 auto;
}

.tool-row {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.tool-row__spacer {
  flex: 1 1 auto;
}

.tool-row__year {
  width: 140px;
}

.tool-row__status {
  width: 140px;
}

/* ==================== 列表 ==================== */

.list-card {
  flex: 1 1 auto;
  min-width: 0;
}

.panel-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 10px;
  flex-wrap: wrap;
}

.panel-head__main {
  display: flex;
  align-items: baseline;
  gap: 10px;
}

.panel-head__title {
  font-size: 14px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.panel-head__meta {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.panel-head__tip {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.list-alert {
  margin-bottom: 10px;
}

.mono {
  font-family: var(--td-font-family-mono, ui-monospace, monospace);
}

.muted {
  color: var(--td-text-color-placeholder);
}

.period-code {
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.current-tag {
  margin-left: 6px;
}

.days {
  margin-left: 4px;
}

/* 当月行：极浅主色底（只做「一眼找到」，不改变行的可读性） */
:deep(.period-row--current) {
  background-color: var(--td-bg-color-container-hover);
}

.panel-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 4px;
  padding: 24px 0;
}

.panel-empty__icon {
  font-size: 28px;
  color: var(--td-text-color-placeholder);
}

.panel-empty__title {
  margin: 0;
  font-size: 14px;
  color: var(--td-text-color-secondary);
}

.panel-empty__desc {
  margin: 0;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}
</style>

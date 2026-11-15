<!--
  折旧计提页（FIN-2 / ARCH-6 §5.10，交互细节③）。

  顶部三行汇总：**本月应提 / 已提 / 差异**。
  - 已提 = 该期间 POSTED 批次的合计（接口值，权威）；
  - 应提 = 按卡片参数逐台测算（口径与后端按月序数推的应提一致，见 asset-logic 注释）；
  - 差异 = 应提 − 已提，非 0 时给方向与排查提示。

  计提 / 红冲都只发请求，金额一律信回包 —— 前端不算折旧入账。
-->
<template>
  <div class="dep-page">
    <!-- ============ 三行汇总（交互细节③） ============ -->
    <div class="summary" data-testid="dep-summary">
      <t-card :bordered="false" class="summary-card">
        <div class="summary-card__label">本月应提（{{ periodLabel(filter.period_code) }}）</div>
        <div class="summary-card__value" data-testid="dep-summary-expected">
          {{ formatMoney(summary.expected_amount) }}
        </div>
        <div class="summary-card__sub">{{ summary.expected_count }} 台在计提范围内</div>
      </t-card>
      <t-card :bordered="false" class="summary-card">
        <div class="summary-card__label">已提</div>
        <div class="summary-card__value" data-testid="dep-summary-posted">
          {{ formatMoney(summary.posted_amount) }}
        </div>
        <div class="summary-card__sub">
          {{ summary.has_posted_batch ? `${summary.posted_count} 台 · 批次已过账` : '该期间还没有已过账批次' }}
        </div>
      </t-card>
      <t-card :bordered="false" class="summary-card">
        <div class="summary-card__label">{{ diff.label }}</div>
        <div class="summary-card__value" :class="`summary-card__value--${diff.theme}`" data-testid="dep-summary-diff">
          {{ diff.value }}
        </div>
        <div class="summary-card__sub">{{ diff.hint }}</div>
      </t-card>
    </div>

    <t-alert
      v-if="summary.skipped_reasons.length > 0"
      theme="warning"
      class="skip-alert"
      data-testid="dep-skip-alert"
    >
      <template #message>
        该期间存在跳过批次：{{ summary.skipped_reasons.join('；') }}
      </template>
    </t-alert>

    <!-- ③④ 主表（批次列表）走 ListPage（P3-12 收口）：服务端分页 + 只读行。汇总卡片留在列表外：它读的是整期数据，不是当前页 -->
    <ListPage
      ref="listRef"
      title="折旧批次"
      testid="dep"
      table-testid="dep-table"
      meta-testid="dep-meta"
      btn-prefix="dep-btn"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      :page-size="PAGE_SIZE"
      @refresh="refreshAux"
    >
      <template #toolbar>
        <t-button
          v-privilege="[PERM.run]"
          theme="primary"
          data-testid="dep-btn-run"
          :loading="running"
          @click="confirmRun"
        >
          <template #icon><t-icon name="play-circle" /></template>
          计提本月折旧
        </t-button>
      </template>

      <template #head-extra>
        <span class="panel-head__tip">
          计提与定时任务走同一个 run_batch（每月 1 日 01:30）；已过账批次可整批红冲，红冲后可重跑复用同一批次行
        </span>
      </template>

      <!--
        期间 / 状态下拉自绘：期间一变，汇总（应提 / 已提 / 差异）要跟着重算，
        `searchOnChange` 只会重拉列表，所以两个控件走 `custom` 插槽自己接 `@change`。
      -->
      <template #filter-period_code>
        <span class="dep-filter__label">会计期间</span>
        <t-select
          v-model="filter.period_code"
          class="dep-filter"
          :options="periodSelectOptions"
          data-testid="dep-filter-period"
          @change="handlePeriodChange"
        />
      </template>

      <template #filter-status>
        <t-select
          v-model="filter.status"
          class="dep-filter dep-filter--narrow"
          :options="statusFilterOptions"
          placeholder="全部状态"
          clearable
          data-testid="dep-filter-status"
          @change="handleStatusChange"
        />
      </template>

      <template #batch_no="{ row }">
        <span class="mono" :data-testid="`dep-no-${row.batch_no}`">{{ row.batch_no }}</span>
      </template>
      <template #status="{ row }">
        <t-tag size="small" variant="light" :theme="depBatchTheme(row.status)">
          {{ depBatchLabel(row.status) }}
        </t-tag>
      </template>
      <template #total_amount="{ row }">
        <span class="money">{{ formatMoney(row.total_amount) }}</span>
      </template>
      <template #voucher_id="{ row }">
        <span class="mono muted">{{ row.voucher_id ? shortId(row.voucher_id) : '—' }}</span>
      </template>
      <template #run_at="{ row }">
        <span class="muted">{{ formatDateTime(row.run_at) }}</span>
      </template>
      <template #op="{ row }">
        <div class="op-cell">
          <t-link theme="primary" :data-testid="`dep-btn-detail-${row.batch_no}`" @click="openDetail(row)">
            明细
          </t-link>
          <t-link
            v-if="canReverseBatch(row)"
            v-privilege="[PERM.reverse]"
            theme="danger"
            :data-testid="`dep-btn-reverse-${row.batch_no}`"
            @click="openReverse(row)"
          >
            红冲
          </t-link>
        </div>
      </template>
    </ListPage>

    <!-- ============ 批次明细 ============ -->
    <t-drawer
      v-model:visible="detailVisible"
      size="860px"
      :header="detail ? `折旧批次 ${detail.batch.batch_no}` : '折旧批次'"
      :footer="false"
      data-testid="dep-detail-drawer"
    >
      <t-loading :loading="detailLoading" size="small">
        <template v-if="detail">
          <t-alert
            v-if="!k4Match"
            theme="warning"
            class="detail-alert"
            message="批次合计与明细合计不一致（K4）：请把这条批次报给后端核对，不要据此入账"
          />
          <t-descriptions :column="3" size="small" bordered>
            <t-descriptions-item label="期间">{{ detail.batch.period_code }}</t-descriptions-item>
            <t-descriptions-item label="状态">{{ depBatchLabel(detail.batch.status) }}</t-descriptions-item>
            <t-descriptions-item label="台数">{{ detail.batch.asset_count }}</t-descriptions-item>
            <t-descriptions-item label="合计金额">
              {{ formatMoney(detail.batch.total_amount) }}
            </t-descriptions-item>
            <t-descriptions-item label="明细合计">
              {{ formatMoney(detail.detail_total) }}
            </t-descriptions-item>
            <t-descriptions-item label="计提凭证">
              <span class="mono">{{ detail.batch.voucher_id || '—' }}</span>
            </t-descriptions-item>
            <t-descriptions-item label="运行时间">{{ formatDateTime(detail.batch.run_at) }}</t-descriptions-item>
            <t-descriptions-item label="过账时间">
              {{ detail.batch.posted_at ? formatDateTime(detail.batch.posted_at) : '—' }}
            </t-descriptions-item>
            <t-descriptions-item label="红冲时间">
              {{ detail.batch.reversed_at ? formatDateTime(detail.batch.reversed_at) : '—' }}
            </t-descriptions-item>
            <t-descriptions-item label="红冲凭证" :span="3">
              <span class="mono">{{ detail.batch.reverse_voucher_id || '—' }}</span>
            </t-descriptions-item>
          </t-descriptions>

          <div class="detail-actions">
            <!--
              VTRACE-4：本批次产生的凭证入口（`DEPRECIATION`，`source_id = biz_depreciation_batch.id`）。
              过账后一张计提凭证；整批红冲后同来源下多一张红字凭证（按钮变成 `(2)` + 「含红字」）。
            -->
            <VoucherTraceButton :sources="voucherSources" testid="dep-detail-vouchers" />
            <t-button
              v-if="canReverseBatch(detail.batch)"
              v-privilege="[PERM.reverse]"
              theme="danger"
              variant="outline"
              data-testid="dep-detail-reverse"
              @click="startReverse(detail.batch)"
            >
              整批红冲
            </t-button>
            <span v-if="detail.batch.skip_reason" class="muted">跳过原因：{{ detail.batch.skip_reason }}</span>
          </div>

          <t-table
            row-key="id"
            :data="detail.details"
            :columns="detailColumns"
            size="small"
            :pagination="{ pageSize: 15, total: detail.details.length }"
            data-testid="dep-detail-table"
          >
            <template #amount="{ row }">
              <span class="money">{{ formatMoney(row.amount) }}</span>
            </template>
            <template #accumulated_before="{ row }">
              <span class="money">{{ formatMoney(row.accumulated_before) }}</span>
            </template>
            <template #accumulated_after="{ row }">
              <span class="money">{{ formatMoney(row.accumulated_after) }}</span>
            </template>
            <template #net_value_after="{ row }">
              <span class="money">{{ formatMoney(row.net_value_after) }}</span>
            </template>
            <template #is_last_month="{ row }">
              <t-tag v-if="row.is_last_month" size="small" theme="warning" variant="light">尾差</t-tag>
              <span v-else class="muted">—</span>
            </template>
          </t-table>
        </template>
      </t-loading>
    </t-drawer>

    <!-- ============ 红冲 ============ -->
    <t-dialog
      v-model:visible="reverseVisible"
      header="整批红冲"
      width="520px"
      :confirm-btn="{ content: '确认红冲', loading: reversing, theme: 'danger' }"
      @confirm="submitReverse"
    >
      <t-alert
        theme="warning"
        message="红冲会生成红字凭证，并把本批次所有卡片的累计折旧回滚到这期之前；之后可以对同一期间重跑（复用同一批次行）。"
      />
      <t-form label-width="92px" class="reverse-form">
        <t-form-item label="批次">
          <span class="mono">{{ reverseTarget?.batch_no }}（{{ reverseTarget?.period_code }}）</span>
        </t-form-item>
        <t-form-item label="红冲原因" :help="reverseError" :status="reverseError ? 'error' : undefined">
          <t-textarea
            v-model="reverseReason"
            :maxlength="255"
            placeholder="至少 2 个字（后端校验）"
            data-testid="dep-reverse-reason"
          />
        </t-form-item>
      </t-form>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';

import { fetchAssetPage, type AssetRow } from '@/api/business/erp/asset';
import {
  fetchDepBatchDetail,
  fetchDepBatchPage,
  reverseDepreciation,
  runDepreciation,
  type DepBatch,
  type DepBatchDetail,
  type DepBatchQuery,
} from '@/api/business/erp/depreciation';
import ListPage from '@/components/ListPage/index.vue';
import type { ColumnDef, FilterDef, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';
import {
  errorText,
  formatDateTime,
  formatMoney,
  summarizeDepreciation,
  validateReverseReason,
  type DepreciationSummary,
} from '../asset/asset-logic';
import {
  canReverseBatch,
  defaultPeriod,
  depBatchLabel,
  depBatchTheme,
  detailTotalMatches,
  diffDisplay,
  periodLabel,
  periodOptions,
  runResultText,
} from './depreciation-logic';
import VoucherTraceButton from '@/views/business/erp/shared/VoucherTraceButton.vue';
import { depreciationSources } from '@/views/business/erp/shared/voucher-trace';

/** 本页权限码（与 V0230 的权限种子逐一对应） */
const PERM = {
  list: 'business:erp:depreciation:list',
  run: 'business:erp:depreciation:run',
  reverse: 'business:erp:depreciation:reverse',
} as const;

const PAGE_SIZE = 20;
/** 汇总口径要把台账拉全（后端上限 100/页，够 3000 台） */
const ASSET_PAGE_SIZE = 100;
const ASSET_MAX_PAGES = 30;

const periodSelectOptions = periodOptions();

const statusFilterOptions = [
  { label: '全部状态', value: '' },
  { label: '已过账', value: 'POSTED' },
  { label: '已红冲', value: 'REVERSED' },
  { label: '已跳过', value: 'SKIPPED' },
  { label: '已计算', value: 'CALCULATED' },
];

/**
 * 筛选行（`filterState` 就地读写）。
 *
 * 期间默认 = 上一个会计月（`defaultPeriod()`，与后端 `run` 不传期间、定时任务同口径）。
 */
const filter = reactive<{ period_code: string; status: string }>({
  period_code: defaultPeriod(),
  status: '',
});

/** 旁路数据：该期间**全部**批次的列表（汇总「已提」用，不是当前页的行） */
const periodBatches = ref<DepBatch[]>([]);
/** 旁路数据：台账卡片（汇总「应提」逐台测算用） */
const assets = ref<AssetRow[]>([]);
const running = ref(false);

const summary = computed<DepreciationSummary>(() =>
  summarizeDepreciation({ assets: assets.value, batches: periodBatches.value, period_code: filter.period_code }),
);
const diff = computed(() => diffDisplay(summary.value));

/** ListPage 实例（计提 / 红冲后重拉主表） */
const listRef = ref<InstanceType<typeof ListPage> | null>(null);

/**
 * 筛选项声明。
 *
 * 两个控件都是 `custom`：期间一变除了列表还要重算汇总，`searchOnChange` 只管列表。
 */
const filters: FilterDef[] = [
  { key: 'period_code', type: 'custom', testid: 'dep-filter-period', width: 150 },
  { key: 'status', type: 'custom', testid: 'dep-filter-status', width: 130 },
];

/** 拉一页（行保持原始 `DepBatch`） */
const fetcher: ListFetcher<DepBatch> = (query) => fetchDepBatchPage(query as DepBatchQuery);

/** 入参拼装（与改造前 `loadBatches` 一致） */
const queryBuilder: ListQueryBuilder = (state, page, pageSize) => {
  const value = state as { period_code: string; status: string };
  return {
    page,
    page_size: pageSize,
    period_code: value.period_code,
    status: value.status as DepBatchQuery['status'],
  };
};

const columns: ColumnDef[] = [
  { colKey: 'batch_no', title: '批次号', width: 190 },
  { colKey: 'period_code', title: '期间', width: 100 },
  { colKey: 'status', title: '状态', width: 100 },
  { colKey: 'asset_count', title: '台数', width: 80, align: 'right' },
  { colKey: 'total_amount', title: '合计金额', width: 140, align: 'right' },
  { colKey: 'voucher_id', title: '计提凭证', width: 140 },
  { colKey: 'run_at', title: '运行时间', width: 160 },
  { colKey: 'op', title: '操作', width: 120, fixed: 'right' },
];

const detailColumns: PrimaryTableCol[] = [
  { colKey: 'asset_no', title: '资产编号', width: 150 },
  { colKey: 'asset_name', title: '名称', width: 160, ellipsis: true },
  { colKey: 'dept_name', title: '部门', width: 110, ellipsis: true },
  { colKey: 'expense_account_code', title: '费用科目', width: 100 },
  { colKey: 'amount', title: '本月折旧', width: 120, align: 'right' },
  { colKey: 'accumulated_before', title: '计提前累计', width: 130, align: 'right' },
  { colKey: 'accumulated_after', title: '计提后累计', width: 130, align: 'right' },
  { colKey: 'net_value_after', title: '计提后净值', width: 130, align: 'right' },
  { colKey: 'is_last_month', title: '尾差', width: 80 },
];

function shortId(id: string): string {
  return id.slice(0, 8);
}

/** 重拉当前页（计提 / 红冲后） */
async function reloadList(): Promise<void> {
  await listRef.value?.reload();
}

/**
 * 该期间的**全部**批次（不受状态筛选影响，供汇总「已提」用）。
 *
 * 与主表分开拉：主表是服务端分页的当前页，汇总要的是整期。
 */
async function loadPeriodBatches(): Promise<void> {
  const page = await fetchDepBatchPage({ page: 1, page_size: 100, period_code: filter.period_code });
  periodBatches.value = page.list ?? [];
}

/** 台账卡片（汇总「应提」按卡片参数逐台测算） */
async function loadAssets(): Promise<void> {
  const out: AssetRow[] = [];
  for (let index = 1; index <= ASSET_MAX_PAGES; index += 1) {
    const page = await fetchAssetPage({ page: index, page_size: ASSET_PAGE_SIZE });
    const list = page.list ?? [];
    out.push(...list);
    if (list.length < ASSET_PAGE_SIZE) {
      break;
    }
  }
  assets.value = out;
}

/**
 * 只刷汇总旁路数据（列表由 ListPage 自己刷，「刷新」按钮同时抛 `@refresh`）。
 * 旁路失败要提示：三个数字会停在旧值，不能静默。
 */
async function refreshAux(): Promise<void> {
  try {
    await Promise.all([loadPeriodBatches(), loadAssets()]);
  } catch (error) {
    MessagePlugin.error(`汇总数据加载失败：${errorText(error)}`);
  }
}

/** 列表 + 汇总一起刷（计提 / 红冲后：两处都会变） */
async function refreshAll(): Promise<void> {
  await Promise.all([reloadList(), refreshAux()]);
}

/** 期间一变：列表回第 1 页 + 汇总重算 */
function handlePeriodChange(): void {
  void listRef.value?.search();
  void refreshAux();
}

/** 状态一变：只重拉列表（汇总口径与本筛选无关） */
function handleStatusChange(): void {
  void listRef.value?.search();
}

// ==================== 计提 ====================

function confirmRun(): void {
  const dialog = DialogPlugin.confirm({
    header: `计提 ${periodLabel(filter.period_code)} 折旧`,
    body: `将按卡片参数计提该期间折旧（与定时任务同一算法）。已计提过会幂等返回「已计提」，不会重复入账。`,
    confirmBtn: '开始计提',
    onConfirm: async () => {
      dialog.hide();
      running.value = true;
      try {
        const result = await runDepreciation({ period_code: filter.period_code });
        MessagePlugin.success(runResultText(result.outcome, result.message, result.total_amount));
        await refreshAll();
      } catch (error) {
        MessagePlugin.error(errorText(error));
      } finally {
        running.value = false;
      }
    },
  });
}

// ==================== 明细 ====================

const detailVisible = ref(false);
const detail = ref<DepBatchDetail | null>(null);
/**
 * VTRACE-4：本批次的凭证来源（`DEPRECIATION`，`source_id = biz_depreciation_batch.id`）。
 *
 * 页面上的「计提凭证」列只有 `voucher_id`（UUID，人看不了也没法点）——
 * 这个按钮才是真正能进到凭证分录的入口。
 */
const voucherSources = computed(() =>
  detail.value ? depreciationSources(detail.value.batch.id, detail.value.batch.batch_no) : [],
);
const detailLoading = ref(false);
const k4Match = computed(() =>
  detail.value ? detailTotalMatches(detail.value.batch, detail.value.detail_total) : true,
);

async function openDetail(row: DepBatch): Promise<void> {
  detailVisible.value = true;
  detailLoading.value = true;
  try {
    detail.value = await fetchDepBatchDetail(row.id);
  } catch (error) {
    MessagePlugin.error(errorText(error));
  } finally {
    detailLoading.value = false;
  }
}

// ==================== 红冲 ====================

const reverseVisible = ref(false);
const reversing = ref(false);
const reverseReason = ref('');
const reverseError = ref('');
const reverseTarget = ref<DepBatch | null>(null);

function openReverse(batch: DepBatch): void {
  startReverse(batch);
}

function startReverse(batch: DepBatch): void {
  reverseTarget.value = batch;
  reverseReason.value = '';
  reverseError.value = '';
  reverseVisible.value = true;
}

async function submitReverse(): Promise<void> {
  const target = reverseTarget.value;
  if (!target) {
    return;
  }
  const error = validateReverseReason(reverseReason.value);
  if (error) {
    reverseError.value = error;
    return;
  }
  reversing.value = true;
  try {
    detail.value = await reverseDepreciation(target.id, reverseReason.value.trim());
    reverseVisible.value = false;
    MessagePlugin.success('已红冲：红字凭证已生成，卡片累计折旧已回滚');
    await refreshAll();
  } catch (error) {
    MessagePlugin.error(errorText(error));
  } finally {
    reversing.value = false;
  }
}

onMounted(() => {
  // 主表由 ListPage 自己拉首屏；这里只补汇总旁路数据（该期间全部批次 + 台账卡片）
  void refreshAux();
});
</script>

<style scoped lang="less">
.dep-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

/* 期间 / 状态下拉（custom 筛选插槽自绘） */
.dep-filter {
  width: 150px;

  &--narrow {
    width: 130px;
  }

  &__label {
    color: var(--td-text-color-secondary);
    font-size: 13px;
  }
}

.summary {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 12px;
}

.summary-card {
  &__label {
    font-size: 12px;
    color: var(--td-text-color-secondary);
    margin-bottom: 6px;
  }

  &__value {
    font-size: 26px;
    font-weight: 700;
    line-height: 1.25;
    font-variant-numeric: tabular-nums;

    &--success {
      color: var(--td-success-color);
    }

    &--warning {
      color: var(--td-warning-color);
    }

    &--danger {
      color: var(--td-error-color);
    }
  }

  &__sub {
    margin-top: 6px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
    line-height: 1.5;
  }
}

.skip-alert {
  margin-top: -4px;
}

/* 抽屉里的预警条（主表现已走 ListPage，列表错误条不在这里） */
.detail-alert {
  margin-bottom: 12px;
}

.op-cell {
  display: flex;
  gap: 10px;
}

.detail-actions {
  display: flex;
  align-items: center;
  gap: 12px;
  margin: 12px 0;
}

.reverse-form {
  margin-top: 12px;
}

.mono {
  font-family: var(--td-font-family-mono, monospace);
  font-size: 13px;
}

.money {
  font-variant-numeric: tabular-nums;
}

.muted {
  color: var(--td-text-color-placeholder);
}
</style>

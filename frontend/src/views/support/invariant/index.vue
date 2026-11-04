<template>
  <div class="invariant-page">
    <!-- 顶部：调度口径 + 手动执行。看板只读，唯一写动作就是「立即执行一次」 -->
    <t-card :bordered="false" class="invariant-page__head">
      <div class="invariant-page__head-main">
        <div>
          <h3 class="invariant-page__title">账务巡检看板</h3>
          <p class="invariant-page__subtitle">
            巡检脚本：<code>{{ latest?.script_path || 'scripts/verify/erp_invariants.py' }}</code>
            · 目标库：<code>{{ latest?.database_name || '—' }}</code>
            · {{ overview?.schedule.description || '内置定时任务每天 02:20 自动执行' }}
            <span v-if="overview"> · cron <code>{{ overview.schedule.cron }}</code></span>
          </p>
        </div>
        <t-space>
          <t-button theme="default" :loading="loading" @click="loadAll">
            <template #icon><t-icon name="refresh" /></template>
            刷新
          </t-button>
          <t-button
            v-privilege="['support:invariant:run']"
            theme="primary"
            :loading="running"
            @click="handleRun"
          >
            <template #icon><t-icon name="play-circle" /></template>
            立即执行一次
          </t-button>
        </t-space>
      </div>
    </t-card>

    <t-loading :loading="loading" size="small" style="width: 100%">
      <t-alert
        v-if="errorMessage"
        theme="error"
        :message="errorMessage"
        class="invariant-page__alert"
      />

      <!-- 指标卡：当前结论 / 通过率 / 连续非 OK / 近 N 天运行次数 -->
      <t-row :gutter="[16, 16]" class="invariant-page__metrics">
        <t-col :xs="12" :md="6" :xl="3">
          <div class="invariant-metric">
            <div class="invariant-metric__label">当前结论</div>
            <div class="invariant-metric__value">
              <t-tag v-if="latest" :theme="statusTheme(latest.status)" variant="light" size="large">
                {{ statusText(latest.status) }}
              </t-tag>
              <span v-else class="invariant-metric__muted">从未跑过</span>
            </div>
            <div class="invariant-metric__extra">
              {{ latest ? `最近一次 ${formatDateTime(latest.run_at)}（${triggerText(latest.trigger_type)}）` : '等待首次执行' }}
            </div>
          </div>
        </t-col>
        <t-col :xs="12" :md="6" :xl="3">
          <div class="invariant-metric">
            <div class="invariant-metric__label">通过 / 总数</div>
            <div class="invariant-metric__value">
              <span v-if="latest">{{ latest.passed }}<span class="invariant-metric__unit">/ {{ latest.total }}</span></span>
              <span v-else class="invariant-metric__muted">—</span>
            </div>
            <div class="invariant-metric__extra">
              <template v-if="latest">
                失败 {{ latest.failed }} · BLOCKER <b :class="{ 'is-danger': latest.blockers > 0 }">{{ latest.blockers }}</b>
                · WARN {{ latest.warns }} · 耗时 {{ latest.duration_ms }}ms
              </template>
              <template v-else>—</template>
            </div>
          </div>
        </t-col>
        <t-col :xs="12" :md="6" :xl="3">
          <div class="invariant-metric">
            <div class="invariant-metric__label">连续非 OK</div>
            <div class="invariant-metric__value">
              <span :class="{ 'is-danger': (overview?.fail_streak ?? 0) > 0 }">{{ overview?.fail_streak ?? 0 }}</span>
              <span class="invariant-metric__unit">次运行</span>
            </div>
            <div class="invariant-metric__extra">
              {{ (overview?.fail_streak ?? 0) > 0 ? '账务还没恢复干净，先看下方失败项' : '最近一次是全绿' }}
            </div>
          </div>
        </t-col>
        <t-col :xs="12" :md="6" :xl="3">
          <div class="invariant-metric">
            <div class="invariant-metric__label">近 {{ overview?.window_days ?? 30 }} 天运行</div>
            <div class="invariant-metric__value">
              {{ overview?.runs_in_window ?? 0 }}<span class="invariant-metric__unit">次</span>
            </div>
            <div class="invariant-metric__extra">每天 02:20 自动 + 手动触发</div>
          </div>
        </t-col>
      </t-row>

      <!-- 趋势：近 N 天通过 / 失败 -->
      <t-card :bordered="false" title="健康度趋势" class="invariant-page__card">
        <template #subtitle>
          <span class="invariant-page__hint">
            每根柱 = 当天最后一次运行（先红后绿与一直红不会看起来一样）；没跑的天不画点
          </span>
        </template>
        <t-alert
          v-if="!trendPoints.length"
          theme="info"
          message="窗口内还没有任何巡检记录 —— 每天 02:20 自动跑一次，或点右上角「立即执行一次」。"
        />
        <EChart v-else :option="trendOption" :height="300" />
      </t-card>

      <!-- 当前失败项 -->
      <t-card :bordered="false" title="当前失败项" class="invariant-page__card">
        <template #subtitle>
          <span class="invariant-page__hint">取最近一次运行的失败项；最近一次全绿时这里为空</span>
        </template>
        <t-alert
          v-if="latest?.status === 'ERROR'"
          theme="warning"
          :message="`本次巡检未跑成（不是账务结论）：${latest.error}`"
        />
        <t-empty v-if="!failures.length" description="没有失败项（或还没有巡检记录）" />
        <t-table
          v-else
          row-key="name"
          :data="failures"
          :columns="failureColumns"
          :hover="true"
          size="small"
        >
          <template #level="{ row }">
            <t-tag :theme="row.level === 'BLOCKER' ? 'danger' : 'warning'" variant="light">
              {{ row.level }}
            </t-tag>
          </template>
          <template #detail="{ row }">
            <span class="invariant-page__detail">{{ row.detail || '—' }}</span>
          </template>
        </t-table>
      </t-card>

      <!-- 运行历史 -->
      <t-card :bordered="false" title="运行历史" class="invariant-page__card">
        <div class="invariant-page__toolbar">
          <t-select v-model="historyFilter" placeholder="全部结论" clearable style="width: 160px" @change="handleFilter">
            <t-option value="OK" label="全绿（OK）" />
            <t-option value="FAIL" label="有失败项（FAIL）" />
            <t-option value="ERROR" label="未跑成（ERROR）" />
          </t-select>
          <span class="invariant-page__hint">共 {{ historyTotal }} 条 · 点行看全量明细</span>
        </div>
        <t-table
          row-key="id"
          :data="historyRows"
          :columns="historyColumns"
          :pagination="pagination"
          :hover="true"
          size="small"
          @page-change="handlePageChange"
          @row-click="openDetail"
        >
          <template #run_at="{ row }">{{ formatDateTime(row.run_at) }}</template>
          <template #status="{ row }">
            <t-tag :theme="statusTheme(row.status)" variant="light">{{ statusText(row.status) }}</t-tag>
          </template>
          <template #trigger_type="{ row }">{{ triggerText(row.trigger_type) }}</template>
          <template #counts="{ row }">{{ row.passed }} / {{ row.total }}</template>
          <template #duration="{ row }">{{ row.duration_ms }} ms</template>
        </t-table>
      </t-card>
    </t-loading>

    <!-- 明细抽屉 -->
    <t-drawer v-model:visible="detailVisible" header="巡检明细" size="720px" :footer="false">
      <t-loading :loading="detailLoading" size="small" style="width: 100%">
        <template v-if="detail">
          <t-descriptions :column="2" size="small" class="invariant-page__desc">
            <t-descriptions-item label="运行时间">{{ formatDateTime(detail.run_at) }}</t-descriptions-item>
            <t-descriptions-item label="结论">
              <t-tag :theme="statusTheme(detail.status)" variant="light">{{ statusText(detail.status) }}</t-tag>
            </t-descriptions-item>
            <t-descriptions-item label="通过 / 总数">{{ detail.passed }} / {{ detail.total }}</t-descriptions-item>
            <t-descriptions-item label="BLOCKER / WARN">{{ detail.blockers }} / {{ detail.warns }}</t-descriptions-item>
            <t-descriptions-item label="触发方式">{{ triggerText(detail.trigger_type) }}</t-descriptions-item>
            <t-descriptions-item label="耗时">{{ detail.duration_ms }} ms</t-descriptions-item>
            <t-descriptions-item label="目标库">{{ detail.database_name }}</t-descriptions-item>
            <t-descriptions-item label="脚本">{{ detail.script_path }}</t-descriptions-item>
          </t-descriptions>
          <t-alert
            v-if="detail.error"
            theme="warning"
            :message="`本次未跑成：${detail.error}`"
            class="invariant-page__alert"
          />
          <t-table
            row-key="name"
            :data="detail.detail"
            :columns="detailColumns"
            :hover="true"
            size="small"
          >
            <template #ok="{ row }">
              <t-tag :theme="row.ok ? 'success' : row.level === 'BLOCKER' ? 'danger' : 'warning'" variant="light">
                {{ row.ok ? '通过' : '失败' }}
              </t-tag>
            </template>
            <template #detail="{ row }">
              <span class="invariant-page__detail">{{ row.detail || '—' }}</span>
            </template>
          </t-table>
        </template>
      </t-loading>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import {
  getInvariantOverview,
  getInvariantResult,
  pageInvariantResults,
  runInvariantAudit,
  type CheckItem,
  type InvariantOverviewRes,
  type InvariantResultDetailRes,
  type InvariantResultRes,
} from '@/api/support/invariant-api';
import { ApiError } from '@/api/request';
import { formatDateTime } from '@/utils/format';
import EChart from '@/components/EChart.vue';

/** 表格分页事件载荷 */
interface TablePageInfo {
  /** 当前页码 */
  current: number;
  /** 每页条数 */
  pageSize: number;
}

/** 看板数据 */
const overview = ref<InvariantOverviewRes | null>(null);
/** 最近一次运行（列表里单独取，便于模板少写可选链） */
const latest = computed<InvariantResultRes | null>(() => overview.value?.latest ?? null);
/** 当前失败项 */
const failures = computed<CheckItem[]>(() => overview.value?.failures ?? []);
/** 趋势点 */
const trendPoints = computed(() => overview.value?.trend ?? []);
/** 加载态（看板） */
const loading = ref(false);
/** 执行态（手动跑巡检） */
const running = ref(false);
/** 接口错误提示 */
const errorMessage = ref('');

/** 运行历史 */
const historyRows = ref<InvariantResultRes[]>([]);
/** 历史总条数 */
const historyTotal = ref(0);
/** 结论过滤（空串 = 全部） */
const historyFilter = ref<string>('');
/** 分页状态 */
const pagination = reactive({ current: 1, pageSize: 10, total: 0 });

/** 明细抽屉 */
const detailVisible = ref(false);
/** 明细加载态 */
const detailLoading = ref(false);
/** 明细内容 */
const detail = ref<InvariantResultDetailRes | null>(null);

/** 当前失败项表格列 */
const failureColumns: PrimaryTableCol[] = [
  { colKey: 'group', title: '分组', width: 90 },
  { colKey: 'name', title: '检查项', width: 260, ellipsis: true },
  { colKey: 'level', title: '级别', width: 100 },
  { colKey: 'detail', title: '差多少 / 差在哪', ellipsis: true },
];

/** 运行历史表格列 */
const historyColumns: PrimaryTableCol[] = [
  { colKey: 'run_at', title: '运行时间', width: 170 },
  { colKey: 'status', title: '结论', width: 110 },
  { colKey: 'trigger_type', title: '触发', width: 90 },
  { colKey: 'counts', title: '通过 / 总数', width: 120 },
  { colKey: 'blockers', title: 'BLOCKER', width: 100 },
  { colKey: 'warns', title: 'WARN', width: 90 },
  { colKey: 'duration', title: '耗时', width: 100 },
];

/** 明细表格列 */
const detailColumns: PrimaryTableCol[] = [
  { colKey: 'group', title: '分组', width: 90 },
  { colKey: 'name', title: '检查项', width: 240, ellipsis: true },
  { colKey: 'level', title: '级别', width: 100 },
  { colKey: 'ok', title: '结果', width: 90 },
  { colKey: 'detail', title: '说明', ellipsis: true },
];

/** 结论 → 主题色 */
function statusTheme(status: string): 'success' | 'danger' | 'warning' | 'default' {
  if (status === 'OK') return 'success';
  if (status === 'FAIL') return 'danger';
  if (status === 'ERROR') return 'warning';
  return 'default';
}

/** 结论 → 中文 */
function statusText(status: string): string {
  if (status === 'OK') return '全绿';
  if (status === 'FAIL') return '有失败项';
  if (status === 'ERROR') return '未跑成';
  return status || '—';
}

/** 触发方式 → 中文 */
function triggerText(trigger: string): string {
  if (trigger === 'SCHEDULED') return '定时';
  if (trigger === 'MANUAL') return '手动';
  return trigger || '—';
}

/** 趋势图配置：通过数（折线）+ 失败数（柱） */
const trendOption = computed(() => {
  const points = trendPoints.value;
  return {
    color: ['#0052d9', '#e34d59'],
    tooltip: {
      trigger: 'axis',
      formatter: (params: Array<{ axisValue: string; seriesName: string; value: number; dataIndex: number }>) => {
        if (!params.length) return '';
        const point = points[params[0].dataIndex];
        if (!point) return '';
        return [
          `${point.date}（运行 ${point.runs} 次，${statusText(point.status)}）`,
          `通过 ${point.passed} / ${point.total}`,
          `失败 ${point.failed}（BLOCKER ${point.blockers} / WARN ${point.warns}）`,
        ].join('<br/>');
      },
    },
    legend: { data: ['通过数', '失败数'], top: 0 },
    grid: { left: 40, right: 24, top: 40, bottom: 30 },
    xAxis: {
      type: 'category',
      data: points.map((point) => point.date.slice(5)),
      axisLabel: { interval: 'auto' },
    },
    yAxis: { type: 'value', minInterval: 1 },
    series: [
      {
        name: '通过数',
        type: 'line',
        smooth: true,
        symbolSize: 6,
        data: points.map((point) => point.passed),
      },
      {
        name: '失败数',
        type: 'bar',
        barMaxWidth: 18,
        data: points.map((point) => point.failed),
      },
    ],
  };
});

/** 加载看板（overview + 运行历史第一页） */
async function loadAll(): Promise<void> {
  loading.value = true;
  errorMessage.value = '';
  try {
    overview.value = await getInvariantOverview(30);
    await loadHistory();
  } catch (error) {
    errorMessage.value = error instanceof ApiError ? error.message : '加载巡检看板失败';
  } finally {
    loading.value = false;
  }
}

/** 加载运行历史当前页 */
async function loadHistory(): Promise<void> {
  const data = await pageInvariantResults({
    page: pagination.current,
    page_size: pagination.pageSize,
    status: historyFilter.value || undefined,
  });
  historyRows.value = data.list;
  historyTotal.value = data.total;
  pagination.total = data.total;
}

/** 结论过滤变化 → 回到第一页重查 */
async function handleFilter(): Promise<void> {
  pagination.current = 1;
  await loadHistory();
}

/** 翻页 */
async function handlePageChange(pageInfo: TablePageInfo): Promise<void> {
  pagination.current = pageInfo.current;
  pagination.pageSize = pageInfo.pageSize;
  await loadHistory();
}

/** 打开某次运行的明细 */
async function openDetail(ctx: { row: Record<string, unknown> }): Promise<void> {
  const row = ctx.row as unknown as InvariantResultRes;
  detailVisible.value = true;
  detailLoading.value = true;
  detail.value = null;
  try {
    detail.value = await getInvariantResult(row.id);
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '加载明细失败');
  } finally {
    detailLoading.value = false;
  }
}

/** 立即执行一次巡检（同步等待结果，最长 300s） */
async function handleRun(): Promise<void> {
  running.value = true;
  try {
    const result = await runInvariantAudit();
    if (result.status === 'OK') {
      MessagePlugin.success(`巡检完成：${result.passed}/${result.total} 全部通过`);
    } else if (result.status === 'ERROR') {
      MessagePlugin.warning(`巡检未跑成：${result.error}`);
    } else {
      MessagePlugin.warning(
        `巡检完成：${result.passed}/${result.total} 通过，BLOCKER ${result.blockers}，WARN ${result.warns}`,
      );
    }
    await loadAll();
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '执行巡检失败');
  } finally {
    running.value = false;
  }
}

onMounted(() => {
  void loadAll();
});
</script>

<style scoped>
.invariant-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.invariant-page__head-main {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
  flex-wrap: wrap;
}

.invariant-page__title {
  margin: 0;
  font-size: 18px;
  font-weight: 600;
}

.invariant-page__subtitle {
  margin: 8px 0 0;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.invariant-page__subtitle code {
  background: var(--td-bg-color-container-hover);
  border-radius: 4px;
  padding: 1px 4px;
}

.invariant-page__alert {
  margin-bottom: 12px;
}

.invariant-page__metrics {
  margin-bottom: 0;
}

.invariant-metric {
  background: var(--td-bg-color-container);
  border-radius: 8px;
  padding: 16px;
  height: 100%;
  box-shadow: var(--td-shadow-1);
}

.invariant-metric__label {
  color: var(--td-text-color-secondary);
  font-size: 13px;
}

.invariant-metric__value {
  margin-top: 6px;
  font-size: 26px;
  font-weight: 600;
  line-height: 1.3;
}

.invariant-metric__unit {
  font-size: 13px;
  color: var(--td-text-color-secondary);
  margin-left: 4px;
  font-weight: 400;
}

.invariant-metric__extra {
  margin-top: 6px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.invariant-metric__muted {
  color: var(--td-text-color-placeholder);
  font-size: 16px;
  font-weight: 400;
}

.is-danger {
  color: var(--td-error-color);
}

.invariant-page__hint {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.invariant-page__toolbar {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 12px;
}

.invariant-page__detail {
  white-space: pre-wrap;
  word-break: break-all;
  font-size: 12px;
}

.invariant-page__desc {
  margin-bottom: 12px;
}
</style>

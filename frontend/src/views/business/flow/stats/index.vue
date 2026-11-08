<template>
  <div class="flow-stats-page">
    <t-card :bordered="false" class="intro-card">
      <h3 class="intro-title">流程统计</h3>
      <p class="page-tip">
        「自审监控」回答「谁在批自己的单、多少钱、有多少被金额上限拦下」；
        「审批总览」看整体通过率与各流程耗时。数据口径全部由后端下发（页面不自己算占比）。
      </p>
    </t-card>

    <t-tabs v-model="activeTab" @change="onTabChange">
      <!-- ==================== 自审监控（P3-05） ==================== -->
      <t-tab-panel value="self" label="自审监控">
        <t-card :bordered="false" class="filter-card">
          <t-row :gutter="[16, 12]">
            <t-col :span="5">
              <t-date-picker
                v-model="month"
                mode="month"
                format="YYYY-MM"
                value-type="YYYY-MM"
                placeholder="选择月份（默认当月）"
                clearable
                data-testid="self-month"
              />
            </t-col>
            <t-col :span="6">
              <t-select
                v-model="bizType"
                :options="bizTypeOpts"
                placeholder="业务类型（全部）"
                clearable
                data-testid="self-biz-type"
              />
            </t-col>
            <t-col :span="13">
              <t-space>
                <t-button theme="primary" data-testid="self-search" @click="loadSelf">
                  <template #icon><t-icon name="search" /></template>
                  查询
                </t-button>
                <t-button variant="outline" :loading="selfLoading" @click="loadSelf">
                  <template #icon><t-icon name="refresh" /></template>
                  刷新
                </t-button>
              </t-space>
            </t-col>
          </t-row>
          <p class="page-tip">
            时间窗：{{ windowText }}（北京时间自然月，左闭右开）。
            当前自审金额上限：
            <strong>{{ thresholdText }}</strong>
            （系统参数 <code>erp.separation_of_duties.self_approval_max_amount</code>，0 = 不限额）。
          </p>
        </t-card>

        <t-alert v-if="selfError" class="list-alert" theme="error" :message="selfError">
          <template #operation>
            <t-button size="small" variant="outline" @click="loadSelf">重试</t-button>
          </template>
        </t-alert>

        <t-row :gutter="[16, 16]" class="stat-row">
          <t-col :span="6">
            <t-card :bordered="false" class="stat-card">
              <div class="stat-label">自审单数</div>
              <div class="stat-value" data-testid="self-count">{{ report?.self_approval_instances ?? 0 }}</div>
              <div class="stat-extra">自审票 {{ report?.self_approval_tickets ?? 0 }} 张</div>
            </t-card>
          </t-col>
          <t-col :span="6">
            <t-card :bordered="false" class="stat-card">
              <div class="stat-label">自审金额合计</div>
              <div class="stat-value" data-testid="self-amount">{{ formatMoney(report?.self_approval_amount) }}</div>
              <div class="stat-extra">这些单据的 form_data 金额之和</div>
            </t-card>
          </t-col>
          <t-col :span="6">
            <t-card :bordered="false" class="stat-card">
              <div class="stat-label">占同期已结算审批单</div>
              <div class="stat-value" data-testid="self-ratio">{{ ratioText }}</div>
              <div class="stat-extra">
                分子 {{ report?.self_approval_instances ?? 0 }} / 分母 {{ report?.settled_instances ?? 0 }}
              </div>
            </t-card>
          </t-col>
          <t-col :span="6">
            <t-card :bordered="false" class="stat-card">
              <div class="stat-label">超上限被降级</div>
              <div class="stat-value" data-testid="self-blocked">{{ report?.blocked_count ?? 0 }}</div>
              <div class="stat-extra">超过上限 → 改走升级链 / 兜底人</div>
            </t-card>
          </t-col>
        </t-row>

        <t-row :gutter="[16, 16]">
          <t-col :span="12">
            <t-card :bordered="false" title="按节点 Top 10">
              <t-table
                row-key="node_key"
                size="small"
                :data="report?.by_node ?? []"
                :columns="nodeColumns"
                :loading="selfLoading"
                :empty="'本月没有自审票'"
                data-testid="self-node-table"
              />
            </t-card>
          </t-col>
          <t-col :span="12">
            <t-card :bordered="false" title="按人 Top 10">
              <t-table
                row-key="user_id"
                size="small"
                :data="report?.by_person ?? []"
                :columns="personColumns"
                :loading="selfLoading"
                :empty="'本月没有自审票'"
                data-testid="self-person-table"
              />
            </t-card>
          </t-col>
        </t-row>

        <t-card :bordered="false" title="自审明细（最近 50 条）">
          <t-table
            row-key="task_id"
            size="small"
            :data="report?.list ?? []"
            :columns="ticketColumns"
            :loading="selfLoading"
            :empty="'本月没有自审票'"
            data-testid="self-ticket-table"
          >
            <template #instance_no="{ row }">
              <t-link theme="primary" @click="openInstance(row.instance_id)">{{ row.instance_no }}</t-link>
            </template>
            <template #amount="{ row }">{{ formatMoney(row.amount) }}</template>
            <template #voted_at="{ row }">{{ formatTime(row.voted_at) }}</template>
          </t-table>
          <p v-if="report?.ratio_note" class="foot-note">口径：{{ report.ratio_note }}</p>
          <p v-if="report?.blocked_note" class="foot-note">拦截计数：{{ report.blocked_note }}</p>
        </t-card>
      </t-tab-panel>

      <!-- ==================== 审批总览（既有三个统计接口） ==================== -->
      <t-tab-panel value="overview" label="审批总览">
        <t-card :bordered="false" class="filter-card">
          <t-row :gutter="[16, 12]">
            <t-col :span="10">
              <t-date-range-picker
                v-model="range"
                format="YYYY-MM-DD"
                value-type="YYYY-MM-DD"
                placeholder="统计区间（默认近 30 天）"
                clearable
                data-testid="overview-range"
              />
            </t-col>
            <t-col :span="14">
              <t-space>
                <t-button theme="primary" data-testid="overview-search" @click="loadOverview">
                  <template #icon><t-icon name="search" /></template>
                  查询
                </t-button>
                <t-button variant="outline" :loading="overviewLoading" @click="loadOverview">
                  <template #icon><t-icon name="refresh" /></template>
                  刷新
                </t-button>
              </t-space>
            </t-col>
          </t-row>
        </t-card>

        <t-alert v-if="overviewError" class="list-alert" theme="error" :message="overviewError" />

        <t-row :gutter="[16, 16]" class="stat-row">
          <t-col :span="4">
            <t-card :bordered="false" class="stat-card">
              <div class="stat-label">审批单总数</div>
              <div class="stat-value">{{ overview?.total ?? 0 }}</div>
            </t-card>
          </t-col>
          <t-col :span="4">
            <t-card :bordered="false" class="stat-card">
              <div class="stat-label">已通过</div>
              <div class="stat-value">{{ overview?.approved ?? 0 }}</div>
            </t-card>
          </t-col>
          <t-col :span="4">
            <t-card :bordered="false" class="stat-card">
              <div class="stat-label">在跑 / 已驳回</div>
              <div class="stat-value">
                {{ overview?.pending ?? 0 }} / {{ overview?.rejected ?? 0 }}
              </div>
            </t-card>
          </t-col>
          <t-col :span="4">
            <t-card :bordered="false" class="stat-card">
              <div class="stat-label">通过率</div>
              <div class="stat-value">{{ percent(overview?.approved_rate) }}</div>
            </t-card>
          </t-col>
          <t-col :span="4">
            <t-card :bordered="false" class="stat-card">
              <div class="stat-label">平均审批时长</div>
              <div class="stat-value">{{ durationText(overview?.avg_duration_seconds) }}</div>
            </t-card>
          </t-col>
          <t-col :span="4">
            <t-card :bordered="false" class="stat-card">
              <div class="stat-label">已结束</div>
              <div class="stat-value">{{ overview?.finished ?? 0 }}</div>
            </t-card>
          </t-col>
        </t-row>

        <t-card :bordered="false" title="各流程效率">
          <t-table
            row-key="flow_code"
            size="small"
            :data="efficiency"
            :columns="efficiencyColumns"
            :loading="overviewLoading"
            :empty="'区间内没有审批单'"
            data-testid="overview-efficiency-table"
          >
            <template #avg_duration_seconds="{ row }">{{ durationText(row.avg_duration_seconds) }}</template>
            <template #max_duration_seconds="{ row }">{{ durationText(row.max_duration_seconds) }}</template>
          </t-table>
        </t-card>
      </t-tab-panel>
    </t-tabs>
  </div>
</template>

<script setup lang="ts">
/**
 * 流程统计（P3-05 新建）：自审监控 + 审批总览。
 *
 * # 为什么这个页面是新建的
 *
 * 后端 `stats/overview` / `stats/efficiency` / `stats/node-bottleneck` 三个接口在 D2 就交付了，
 * 但前端**从来没有过统计页**（零调用方）。P3-05 的卡面要求「流程统计页加自审监控 tab」，
 * 所以这里一并把统计页建起来：Tab 1 是新的自审监控，Tab 2 复用既有总览 / 效率接口。
 *
 * # 三条硬口径
 *
 * 1. **金额一律按字符串展示**（后端 numeric 走 `::text`，JS number 会丢精度）；
 * 2. **占比不自己算**：`ratio` 与 `ratio_note` 都由后端下发（分子 / 分母定义写在后端）；
 * 3. **接口失败必须显示错误态**，不许静默空表（红线）。
 */
import { computed, onMounted, ref } from 'vue';
import { useRouter } from 'vue-router';
import type { PrimaryTableCol } from 'tdesign-vue-next';
import {
  getFlowSelfApprovalStats,
  getFlowStatsEfficiency,
  getFlowStatsOverview,
  type FlowSelfApprovalReport,
  type FlowStatsEfficiencyRow,
  type FlowStatsOverview,
} from '@/api/business/flow-api';
import { ApiError } from '@/api/request';
import { FLOW_BIZ_TYPES, FLOW_BIZ_TYPE_LABELS } from '@/api/business/erp/doc-kinds';

const router = useRouter();

/** 当前 Tab（自审监控默认） */
const activeTab = ref<'self' | 'overview'>('self');

// ==================== 自审监控 ====================

/** 月份 `YYYY-MM`（空串 = 后端按当月） */
const month = ref<string>('');
/** 业务类型（空串 = 全部） */
const bizType = ref<string>('');
/** 报表 */
const report = ref<FlowSelfApprovalReport | null>(null);
/** 加载中 */
const selfLoading = ref(false);
/** 错误文案（真接口失败时显示） */
const selfError = ref('');

/** 业务类型下拉（词表来自 doc-kinds.ts 的唯一真相） */
const bizTypeOpts = FLOW_BIZ_TYPES.map((value) => ({
  label: `${FLOW_BIZ_TYPE_LABELS[value]}（${value}）`,
  value,
}));

/** 时间窗文案（RFC3339 → 本地可读） */
const windowText = computed(() => {
  if (!report.value) {
    return '—';
  }
  return `${formatTime(report.value.start, true)} ~ ${formatTime(report.value.end, true)}`;
});

/** 自审上限文案 */
const thresholdText = computed(() => {
  const value = report.value?.threshold ?? 0;
  return value > 0 ? `${formatMoney(String(value))} 元` : '不限额';
});

/** 占比文案（后端给 0–1，展示成百分比；分母为 0 时后端给 0.0） */
const ratioText = computed(() => percent(report.value?.ratio));

/** 按节点列 */
const nodeColumns: PrimaryTableCol[] = [
  { colKey: 'node_name', title: '节点', ellipsis: true },
  { colKey: 'tickets', title: '自审票', width: 100 },
  { colKey: 'instances', title: '涉及单据', width: 110 },
  { colKey: 'amount', title: '金额合计', width: 140 },
];

/** 按人列 */
const personColumns: PrimaryTableCol[] = [
  { colKey: 'assignee_name', title: '审批人', width: 140 },
  { colKey: 'tickets', title: '自审票', width: 100 },
  { colKey: 'instances', title: '涉及单据', width: 110 },
  { colKey: 'amount', title: '金额合计', width: 140 },
];

/** 明细列 */
const ticketColumns: PrimaryTableCol[] = [
  { colKey: 'instance_no', title: '单据号', width: 200 },
  { colKey: 'title', title: '标题', ellipsis: true },
  { colKey: 'node_name', title: '节点', width: 160 },
  { colKey: 'assignee_name', title: '审批人', width: 120 },
  { colKey: 'amount', title: '金额', width: 140 },
  { colKey: 'voted_at', title: '自审时间', width: 170 },
];

// ==================== 审批总览 ====================

/** 统计区间（`YYYY-MM-DD`，空 = 后端近 30 天） */
const range = ref<string[]>([]);
/** 总览 */
const overview = ref<FlowStatsOverview | null>(null);
/** 各流程效率 */
const efficiency = ref<FlowStatsEfficiencyRow[]>([]);
/** 加载中 */
const overviewLoading = ref(false);
/** 错误文案 */
const overviewError = ref('');

/** 效率列 */
const efficiencyColumns: PrimaryTableCol[] = [
  { colKey: 'flow_name', title: '流程', ellipsis: true },
  { colKey: 'total', title: '总数', width: 90 },
  { colKey: 'approved', title: '通过', width: 90 },
  { colKey: 'rejected', title: '驳回', width: 90 },
  { colKey: 'running', title: '在跑', width: 90 },
  { colKey: 'avg_duration_seconds', title: '平均时长', width: 140 },
  { colKey: 'max_duration_seconds', title: '最长时长', width: 140 },
];

// ==================== 工具 ====================

/** 后端错误文案（`ApiError.message` 已经是后端中文 `msg`） */
function errorMessageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/**
 * 金额展示。
 *
 * 后端下发的是**字符串**（`numeric::text`）：能解析就补两位小数，解析不了原样显示
 * —— 绝不 `Number()` 后 `toFixed`，那会把大额金额的四舍五入悄悄改掉。
 */
function formatMoney(raw: string | number | undefined): string {
  if (raw === undefined || raw === null || raw === '') {
    return '0.00';
  }
  const text = String(raw);
  /** 只允许 `-?digits(.digits)?` 走格式化，别的原样返回 */
  if (!/^-?\d+(\.\d+)?$/.test(text)) {
    return text;
  }
  const [integer, decimal = ''] = text.split('.');
  const sign = integer.startsWith('-') ? '-' : '';
  const digits = sign ? integer.slice(1) : integer;
  const padded = `${decimal}00`.slice(0, 2);
  return `${sign}${digits}.${padded}`;
}

/** 0–1 → 百分比（缺失 / 非法 → 0%） */
function percent(value: number | undefined): string {
  if (value === undefined || value === null || !Number.isFinite(value)) {
    return '0.00%';
  }
  return `${(value * 100).toFixed(2)}%`;
}

/** 秒 → 人话时长 */
function durationText(seconds: number | undefined): string {
  if (!seconds || seconds <= 0) {
    return '—';
  }
  if (seconds < 60) {
    return `${seconds} 秒`;
  }
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) {
    return `${minutes} 分钟`;
  }
  const hours = Math.floor(minutes / 60);
  if (hours < 24) {
    return `${hours} 小时 ${minutes % 60} 分`;
  }
  return `${Math.floor(hours / 24)} 天 ${hours % 24} 小时`;
}

/** RFC3339 → `YYYY-MM-DD HH:mm`（`dateOnly` 只显示日期） */
function formatTime(iso: string | undefined, dateOnly = false): string {
  if (!iso) {
    return '—';
  }
  const date = new Date(iso);
  if (!Number.isFinite(date.getTime())) {
    return '—';
  }
  const pad = (value: number): string => String(value).padStart(2, '0');
  const day = `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
  return dateOnly ? day : `${day} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** 打开审批单详情 */
function openInstance(id: string): void {
  void router.push(`/flow/instance/${id}`);
}

/** 拉自审监控 */
async function loadSelf(): Promise<void> {
  selfLoading.value = true;
  selfError.value = '';
  try {
    report.value = await getFlowSelfApprovalStats({
      month: month.value || undefined,
      biz_type: bizType.value || undefined,
    });
  } catch (error) {
    report.value = null;
    selfError.value = `加载自审监控失败：${errorMessageOf(error)}`;
  } finally {
    selfLoading.value = false;
  }
}

/** 拉审批总览 + 效率（同一次点击打两个接口，任一失败都显示错误） */
async function loadOverview(): Promise<void> {
  overviewLoading.value = true;
  overviewError.value = '';
  const query = {
    start: range.value?.[0] || undefined,
    end: range.value?.[1] || undefined,
  };
  try {
    const [summary, detail] = await Promise.all([
      getFlowStatsOverview(query),
      getFlowStatsEfficiency(query),
    ]);
    overview.value = summary;
    efficiency.value = detail.list;
  } catch (error) {
    overview.value = null;
    efficiency.value = [];
    overviewError.value = `加载审批总览失败：${errorMessageOf(error)}`;
  } finally {
    overviewLoading.value = false;
  }
}

/** 切 Tab：首次进入该 Tab 才拉数据（避免打开页面就打两套接口） */
function onTabChange(value: string | number): void {
  if (value === 'overview' && overview.value === null && !overviewLoading.value) {
    void loadOverview();
  }
  if (value === 'self' && report.value === null && !selfLoading.value) {
    void loadSelf();
  }
}

onMounted(() => {
  void loadSelf();
});
</script>

<style scoped>
.flow-stats-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.intro-card .intro-title {
  margin: 0 0 6px;
  font-size: 16px;
  font-weight: 600;
}

.page-tip {
  margin: 8px 0 0;
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 1.7;
}

.list-alert {
  margin-bottom: 12px;
}

.stat-row {
  margin-bottom: 16px;
}

.stat-card {
  height: 100%;
}

.stat-label {
  color: var(--td-text-color-secondary);
  font-size: 13px;
}

.stat-value {
  margin: 6px 0 4px;
  font-size: 24px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.stat-extra {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.foot-note {
  margin: 10px 0 0;
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 1.7;
}

code {
  padding: 0 4px;
  border-radius: 3px;
  background: var(--td-bg-color-secondarycontainer);
  font-size: 12px;
}
</style>

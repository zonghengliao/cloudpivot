<template>
  <div class="burndown">
    <div class="burndown__bar">
      <t-space align="center">
        <span class="burndown__title">燃尽图</span>
        <t-radio-group v-model="board" variant="default-filled" size="small" @change="load">
          <t-radio-button value="all">全部可见任务</t-radio-button>
          <t-radio-button value="my">只看我参与</t-radio-button>
        </t-radio-group>
        <t-date-range-picker
          v-model="range"
          clearable
          format="YYYY-MM-DD"
          value-type="YYYY-MM-DD"
          placeholder="区间（默认按任务的首末日期）"
          @change="load"
        />
        <t-button variant="outline" size="small" :loading="loading" @click="load">
          <template #icon><t-icon name="refresh" /></template>
          刷新
        </t-button>
      </t-space>
      <t-space align="center" size="large">
        <span class="burndown__stat">任务 {{ data?.total ?? 0 }}</span>
        <span class="burndown__stat">已完成 {{ data?.completed ?? 0 }}</span>
        <span v-if="data" class="burndown__range">{{ data.start }} ~ {{ data.end }}</span>
      </t-space>
    </div>

    <t-alert v-if="errorMessage" theme="error" :message="errorMessage">
      <template #operation>
        <t-button size="small" variant="outline" @click="load">重试</t-button>
      </template>
    </t-alert>

    <t-loading :loading="loading" size="small">
      <div v-if="data && data.dates.length" class="burndown__chart">
        <EChart :option="option" :height="360" />
        <p class="burndown__legend">
          理想线：从起始日线性递减到 0；实际线：每日开始时剩余的未完成任务数（按完成时间反推，CST）
        </p>
      </div>
      <div v-else-if="!loading" class="burndown__empty">
        <t-icon class="burndown__empty-icon" name="chart-line" />
        <p>这段时间没有任务可画 —— 换个项目 / 区间试试</p>
      </div>
    </t-loading>
  </div>
</template>

<script setup lang="ts">
/**
 * 燃尽图（批次 E · E11 三、只读）。
 *
 * 数据来自 `GET /api/business/task/burndown`（后端算好三条线，前端只画）。
 * 图表用项目已有的 `@/components/EChart.vue`（内部已接配色引擎，别自己 `echarts.init`）。
 */
import { computed, onMounted, ref, watch } from 'vue';
import type { EChartsCoreOption } from 'echarts';
import EChart from '@/components/EChart.vue';
import { getTaskBurndown, type TaskBurndown } from '@/api/business/task-api';
import { ApiError } from '@/api/request';

const props = defineProps<{
  /** 项目 id（父页面筛选区选择；空 = 不限项目） */
  projectId?: string | null;
}>();

/** 看板维度：all = 全部可见 / my = 只看我参与 */
const board = ref<'all' | 'my'>('all');
/** 日期区间（空数组 = 用后端默认） */
const range = ref<string[]>([]);
/** 后端数据 */
const data = ref<TaskBurndown | null>(null);
/** 加载中 */
const loading = ref(false);
/** 错误文案 */
const errorMessage = ref('');

/** ECharts option（理想线虚线 + 实际线实线） */
const option = computed<EChartsCoreOption>(() => {
  const dates = data.value?.dates ?? [];
  const ideal = data.value?.ideal ?? [];
  const actual = data.value?.actual ?? [];
  return {
    tooltip: { trigger: 'axis' },
    legend: { data: ['理想线', '实际线'], top: 0 },
    grid: { left: 48, right: 24, top: 40, bottom: 40 },
    xAxis: {
      type: 'category',
      boundaryGap: false,
      data: dates,
      axisLabel: { formatter: (value: string) => value.slice(5) },
    },
    yAxis: { type: 'value', name: '剩余任务', minInterval: 1 },
    series: [
      {
        name: '理想线',
        type: 'line',
        smooth: false,
        symbol: 'none',
        lineStyle: { type: 'dashed', width: 2 },
        data: ideal,
      },
      {
        name: '实际线',
        type: 'line',
        smooth: false,
        symbolSize: 6,
        areaStyle: { opacity: 0.12 },
        data: actual,
      },
    ],
  };
});

/** 拉数据 */
async function load(): Promise<void> {
  loading.value = true;
  errorMessage.value = '';
  try {
    const query: { project_id?: string; board?: 'all' | 'my'; start?: string; end?: string } = {
      board: board.value,
    };
    if (props.projectId) {
      query.project_id = props.projectId;
    }
    if (range.value?.[0]) {
      query.start = range.value[0];
    }
    if (range.value?.[1]) {
      query.end = range.value[1];
    }
    data.value = await getTaskBurndown(query);
  } catch (error) {
    errorMessage.value = error instanceof ApiError ? error.message : '燃尽图加载失败';
    data.value = null;
  } finally {
    loading.value = false;
  }
}

watch(
  () => props.projectId,
  () => {
    void load();
  },
);

onMounted(() => {
  void load();
});
</script>

<style scoped>
.burndown {
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 8px 4px;
}

.burndown__bar {
  display: flex;
  justify-content: space-between;
  align-items: center;
  flex-wrap: wrap;
  gap: 12px;
}

.burndown__title {
  font-weight: 600;
  font-size: 15px;
}

.burndown__stat {
  color: var(--td-text-color-secondary);
  font-size: 13px;
}

.burndown__range {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.burndown__legend {
  margin: 4px 0 0;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.burndown__empty {
  padding: 48px 0;
  text-align: center;
  color: var(--td-text-color-secondary);
}

.burndown__empty-icon {
  font-size: 40px;
  color: var(--td-text-color-placeholder);
}
</style>

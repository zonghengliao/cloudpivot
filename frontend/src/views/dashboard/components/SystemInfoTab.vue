<template>
  <div class="si">
    <!-- 概览接口失败提示：图表降级为空图时必须让用户知道原因 -->
    <t-alert v-if="overviewError" theme="warning" :message="overviewError" class="si__alert" />

    <!-- ① 系统健康摘要（4 指标 + 查看详情 → 服务器状态页） -->
    <HealthSummaryCard class="si__block" />

    <!--
      ② 系统概况（今日登录 / 在线用户 / 登录失败率 / 启用任务 全在这里）。
      这 8 张卡从原首页整体搬来：它们全是**全系统口径**的运维数字（用户总数、角色/菜单数、
      操作日志量…），不适合给非超管看，所以只留在这个 Tab 里，工作台一张都不留。
    -->
    <section class="si__block">
      <div class="si__section-head">
        <h3 class="si__section-title">系统概况</h3>
        <span class="si__section-hint">
          全系统口径 · 含今日登录 / 在线用户 / 失败率 / 启用任务
        </span>
      </div>

      <t-loading :loading="isLoading" size="small" style="width: 100%">
        <t-row :gutter="[16, 16]">
          <!--
            一行 4 个：TDesign Grid 是 **12 列制**（t-col-{断点}-N = 每行 12/N 个），
            故一行 4 个取 :xl="3"；随屏宽逐级降为 3 / 2 个。
          -->
          <t-col
            v-for="card in visibleMetrics"
            :key="card.key"
            :xs="6"
            :sm="4"
            :md="3"
            :xl="3"
            class="si__metric-col"
          >
            <div class="si__metric">
              <div class="si__metric-head">
                <span class="si__metric-icon" :class="`si__metric-icon--${iconTheme(card.key)}`">
                  <t-icon :name="iconName(card)" />
                </span>
                <span class="si__metric-label">{{ card.label }}</span>
              </div>
              <div class="si__metric-value">
                {{ formatValue(card) }}
                <span v-if="card.unit" class="si__metric-unit">{{ card.unit }}</span>
              </div>
              <div class="si__metric-extra">
                <span v-if="card.key === 'fail_rate' && card.extra > 0">
                  共 {{ card.extra }} 次登录尝试
                </span>
                <span v-else-if="card.key === 'job_total'">
                  启用 {{ card.extra }} / {{ card.value }}
                </span>
                <span v-else-if="card.key === 'login_today' && card.extra >= 0">
                  失败 {{ card.extra }} 次
                </span>
              </div>
              <t-tooltip :content="card.tip">
                <span class="si__metric-tip">口径说明</span>
              </t-tooltip>
            </div>
          </t-col>
        </t-row>
        <t-empty v-if="!isLoading && !visibleMetrics.length" description="暂无系统概况数据" />
      </t-loading>
    </section>

    <!-- ③ 两张趋势图并排 -->
    <t-row :gutter="[16, 16]" class="si__block">
      <t-col class="si__half-col">
        <t-card :title="loginTrendTitle" :bordered="false" class="si__card">
          <t-loading :loading="isLoading" size="small">
            <EChart v-if="!overviewError" :height="260" :option="loginTrendOption" />
            <t-empty v-else description="暂无趋势数据" />
          </t-loading>
        </t-card>
      </t-col>

      <t-col class="si__half-col">
        <t-card :title="latencyTrendTitle" :bordered="false" class="si__card">
          <t-loading :loading="isLoading" size="small">
            <EChart v-if="!overviewError" :height="260" :option="latencyTrendOption" />
            <t-empty v-else description="暂无趋势数据" />
          </t-loading>
        </t-card>
      </t-col>
    </t-row>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import type { EChartsCoreOption } from 'echarts';
import EChart from '@/components/EChart.vue';
import HealthSummaryCard from './HealthSummaryCard.vue';
import { fetchOverview, type OverviewRes } from '@/api/support/dashboard-api';
import { ApiError } from '@/api/request';

/** 系统概览数据，加载中或未加载时为 null */
const overview = ref<OverviewRes | null>(null);
/** 概览加载中标记 */
const isLoading = ref(false);
/** 概览加载失败提示（不阻断页面渲染，图表降级为空图） */
const overviewError = ref('');

/**
 * 概览里展示的 8 个核心指标（后端共下发 12 个）。
 *
 * 挑选原则：覆盖「人（用户/角色）— 行为（登录/在线）— 内容量（菜单）— 运维（任务）— 审计（日志）」，
 * 数量再少就偏科、更多则一屏放不下（每行 4 个 × 2 行）。顺序即展示顺序；
 * 后端未下发的键会被自动跳过。
 *
 * 注：`avg_login_ms`（平均登录耗时）已移出——下方「登录耗时趋势」图已表达该口径，
 * 卡片与图表重复；换入 `op_log_week` 补上审计维度。
 */
const METRIC_KEYS = [
  'user_total',
  'online_user',
  'login_today',
  'fail_rate',
  'op_log_week',
  'role_total',
  'menu_total',
  'job_total',
] as const;

/** 按 METRIC_KEYS 过滤后的指标卡片 */
const visibleMetrics = computed(() => {
  const all = overview.value?.metrics ?? [];
  const picked = METRIC_KEYS.map((key) => all.find((m) => m.key === key)).filter(
    (m): m is NonNullable<typeof m> => !!m,
  );
  // 后端调整了指标集合时兜底：不足 8 个就按原顺序补齐，避免出现空行
  return picked.length === METRIC_KEYS.length
    ? picked
    : [...picked, ...all.filter((m) => !METRIC_KEYS.includes(m.key as never))].slice(0, 8);
});

/** 趋势窗口天数：概览未就绪时按后端默认 14 天占位 */
const trendDays = computed(() => overview.value?.trend_days ?? 14);

/** 卡片标题必须走绑定：写在静态属性里的 {{ }} 不会被 Vue 插值，会原样显示 */
const loginTrendTitle = computed(() => `登录趋势（近 ${trendDays.value} 天）`);
const latencyTrendTitle = computed(() => `登录耗时趋势（近 ${trendDays.value} 天）`);

/** 指标大数降级：超过 10000 显示为「1.2万」，避免卡片被撑破 */
function formatValue(card: OverviewRes['metrics'][number]): string {
  if (card.value >= 10000) return `${(card.value / 10000).toFixed(1)}万`;
  return String(card.value);
}

/**
 * 后端下发的图标名 → TDesign 实际存在的图标名。
 *
 * 后端沿用了早期约定的 `user-check` / `speed`，但 TDesign 图标集里
 * 没有这两个（正确写法是 `user-checked` 与 `rocket`），`t-icon` 找不到时
 * 静默渲染成空白图标——这里在展示层兜底纠正，不必改后端协议。
 */
const ICON_NAME_FIX: Record<string, string> = {
  'user-check': 'user-checked',
  speed: 'rocket',
};

/** 取可渲染的图标名 */
function iconName(card: OverviewRes['metrics'][number]): string {
  return ICON_NAME_FIX[card.icon] ?? card.icon;
}

/**
 * 图标底色只保留 3 种（蓝 / 绿 / 橙），按指标 key 稳定取模分配。
 *
 * 卡片本身统一底色、不再带左侧色条，色彩只作为「图标点色」使用，
 * 避免一屏 8 张卡出现 6 种颜色造成视觉噪音。
 */
const ICON_THEMES = ['blue', 'green', 'orange'] as const;
const ICON_THEME_BY_KEY: Record<string, (typeof ICON_THEMES)[number]> = {
  user_total: 'blue',
  online_user: 'green',
  role_total: 'blue',
  active_today: 'green',
  login_today: 'blue',
  menu_total: 'orange',
  dept_total: 'green',
  fail_rate: 'orange',
  avg_login_ms: 'blue',
  dict_type_total: 'orange',
  job_total: 'green',
  op_log_week: 'orange',
};

/** 取图标底色键（未登记的指标按 key 稳定散列到 3 色之一） */
function iconTheme(key: string): (typeof ICON_THEMES)[number] {
  const preset = ICON_THEME_BY_KEY[key];
  if (preset) return preset;
  let sum = 0;
  for (const ch of key) sum += ch.charCodeAt(0);
  return ICON_THEMES[sum % ICON_THEMES.length];
}

/** 登录趋势图配置：成功 / 失败两条折线 + 成功线面积填充 */
const loginTrendOption = computed<EChartsCoreOption>(() => {
  const trend = overview.value?.login_trend;
  return {
    grid: { left: 48, right: 24, top: 40, bottom: 32 },
    tooltip: { trigger: 'axis' },
    legend: { data: trend?.lines.map((line) => line.name) ?? [], top: 0 },
    xAxis: {
      type: 'category',
      boundaryGap: false,
      data: trend?.dates ?? [],
      axisLabel: { hideOverlap: true },
    },
    yAxis: { type: 'value', minInterval: 1 },
    series: (trend?.lines ?? []).map((line, index) => ({
      name: line.name,
      type: 'line',
      smooth: true,
      showSymbol: false,
      data: line.data,
      // 不写死颜色：由 EChart 组件注入当前配色方案的图表色（切换配色时自动重绘）
      areaStyle: index === 0 ? { opacity: 0.15 } : undefined,
    })),
  };
});

/** 登录耗时趋势图配置：平均耗时单折线，空缺日画断点 */
const latencyTrendOption = computed<EChartsCoreOption>(() => {
  const trend = overview.value?.latency_trend;
  return {
    grid: { left: 48, right: 24, top: 40, bottom: 32 },
    tooltip: { trigger: 'axis', valueFormatter: (value: number) => `${value} ms` },
    legend: { data: trend?.lines.map((line) => line.name) ?? [], top: 0 },
    xAxis: {
      type: 'category',
      boundaryGap: false,
      data: trend?.dates ?? [],
      axisLabel: { hideOverlap: true },
    },
    yAxis: { type: 'value', name: 'ms', nameTextStyle: { padding: [0, 0, 0, 24] } },
    series: (trend?.lines ?? []).map((line) => ({
      name: line.name,
      type: 'line',
      smooth: true,
      // 无样本日为 null，断开连线而不是补 0 造成「假趋势」
      connectNulls: false,
      data: line.data,
      areaStyle: { opacity: 0.12 },
    })),
  };
});

/** 拉取系统概览：失败只提示，不阻塞健康摘要卡 */
async function loadOverview(): Promise<void> {
  isLoading.value = true;
  overviewError.value = '';
  try {
    overview.value = await fetchOverview();
  } catch (error) {
    overview.value = null;
    overviewError.value = error instanceof ApiError ? error.message : '概览数据加载失败';
  } finally {
    isLoading.value = false;
  }
}

onMounted(() => {
  void loadOverview();
});
</script>

<style scoped lang="less">
.si {
  // 各区块之间统一 16px 间距（与页面其它卡片一致）
  &__block + &__block,
  &__alert + &__block {
    margin-top: 16px;
  }

  &__alert {
    margin-bottom: 16px;
  }

  &__section-head {
    display: flex;
    gap: 10px;
    align-items: baseline;
    margin: 0 0 10px 2px;
  }

  &__section-title {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__section-hint {
    overflow: hidden;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  // 半宽列（两张趋势图）：中屏及以上各占 50%，窄屏回落整行。
  // 不用 t-col 的断点 props —— 那会写成 inline style，媒体查询压不住。
  &__half-col {
    flex: 1 1 100%;
    width: 100%;
    min-width: 0;
    max-width: 100%;

    @media (min-width: 992px) {
      flex: 1 1 50%;
      width: 50%;
      max-width: 50%;
    }
  }

  &__card {
    height: 100%;
  }

  // ---------- 指标卡片（沿用原首页口径，整体搬入「系统信息」） ----------
  &__metric-col {
    // 栅格列只提供位置，卡片高度由自身撑开
    display: flex;
  }

  &__metric {
    display: flex;
    flex: 1;
    flex-direction: column;
    min-width: 0;
    padding: 16px;
    // 统一底色 + 统一描边：不随指标变化，也不再有左侧色条
    background-color: var(--td-bg-color-container);
    border: 1px solid var(--td-component-border);
    border-radius: 10px;
    transition: box-shadow 0.2s ease, transform 0.2s ease;

    &:hover {
      box-shadow: 0 4px 14px rgb(0 0 0 / 8%);
      transform: translateY(-1px);
    }
  }

  &__metric-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  // 图标底色只保留 3 种（见 script 的 ICON_THEMES），白字 + 圆角方块
  &__metric-icon {
    display: inline-flex;
    flex: none;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    font-size: 16px;
    color: #fff;
    background-color: var(--metric-color, var(--td-brand-color-7));
    border-radius: 8px;

    &--green {
      --metric-color: #3e9b72;
    }

    &--orange {
      --metric-color: #d98b4a;
    }

    &--blue {
      --metric-color: var(--td-brand-color-7);
    }
  }

  &__metric-label {
    font-size: 13px;
    color: var(--td-text-color-secondary);
  }

  &__metric-value {
    margin-top: 10px;
    font-size: 26px;
    font-weight: 600;
    line-height: 1.2;
    color: var(--td-text-color-primary);
  }

  &__metric-unit {
    margin-left: 2px;
    font-size: 13px;
    font-weight: 400;
    color: var(--td-text-color-secondary);
  }

  &__metric-extra {
    min-height: 20px;
    margin-top: 4px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__metric-tip {
    margin-top: auto;
    padding-top: 6px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
    text-decoration: underline dotted;
    cursor: help;
  }
}
</style>

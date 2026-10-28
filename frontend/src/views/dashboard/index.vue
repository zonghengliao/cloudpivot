<template>
  <div class="dashboard">
    <!-- 欢迎横幅：登录后首屏的视觉锚点 -->
    <section class="dashboard__banner">
      <div class="dashboard__banner-text">
        <h2 class="dashboard__greeting">{{ greeting }}，{{ userStore.displayName || '管理员' }}</h2>
        <p class="dashboard__subtitle">今天是 {{ today }}，祝你工作顺利。</p>
      </div>
      <t-icon class="dashboard__banner-icon" name="dashboard" />
    </section>

    <t-loading :loading="isLoading" size="small" style="width: 100%">
      <!--
        外层栅格：右侧固定 360px，左侧自适应。
        注意两个必需项——左栏 min-width:0（否则 flex item 的 min-width:auto 会被
        内部宽内容撑开，把右栏挤到下一行）；t-row 默认 flex-wrap:wrap，
        窄屏时靠媒体查询在 <1200px 切换为上下堆叠（见样式中的 dashboard__body）。
      -->
      <t-row :gutter="[16, 16]" class="dashboard__body" align="stretch">
        <!-- 左栏：统计卡片 / 快捷入口 / 两张趋势图 -->
        <t-col class="dashboard__main">
          <!-- 概览接口失败提示：图表降级为空图时必须让用户知道原因 -->
          <t-alert v-if="overviewError" theme="warning" :message="overviewError" class="dashboard__alert" />

          <t-row :gutter="[16, 16]">
            <!--
              第 1 行：统计卡片。TDesign Grid 是 **12 列制**（t-col-{断点}-N = 每行 12/N 个），
              故一行 4 个取 :xl="3"；随屏宽逐级降为 3 / 2 个。
            -->
            <t-col
              v-for="card in visibleMetrics"
              :key="card.key"
              :xs="6"
              :sm="4"
              :md="3"
              :xl="3"
              class="dashboard__metric-col"
            >
              <div class="dashboard__metric">
                <div class="dashboard__metric-head">
                  <span class="dashboard__metric-icon" :class="`dashboard__metric-icon--${iconTheme(card.key)}`">
                    <t-icon :name="iconName(card)" />
                  </span>
                  <span class="dashboard__metric-label">{{ card.label }}</span>
                </div>
                <div class="dashboard__metric-value">
                  {{ formatValue(card) }}
                  <span v-if="card.unit" class="dashboard__metric-unit">{{ card.unit }}</span>
                </div>
                <div class="dashboard__metric-extra">
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
                  <span class="dashboard__metric-tip">口径说明</span>
                </t-tooltip>
              </div>
            </t-col>
          </t-row>

          <!--
            第 2 行：快捷入口。5 无法被 12 整除，因此大屏用 flex-basis:20% 强制一行 5 个；
            窄屏回落到 12 列制分栏（4 / 3 / 2 个）。
            外层必须用 t-col 包住 t-card —— t-card 直接放进 t-row 会被 flex 压缩成内容宽度。
            注意 TDesign Grid 是 12 列制，没有 t-col-xs-24（写 24 是无效类名），整行要用 12。
          -->
          <t-row :gutter="[16, 16]" class="dashboard__row">
            <t-col class="dashboard__full-col">
              <t-card title="快捷入口" :bordered="false" class="dashboard__card">
                <t-loading :loading="isLoading" size="small">
                  <t-row
                    v-if="visibleEntries.length"
                    :gutter="[12, 12]"
                    tag="div"
                    class="dashboard__entries"
                  >
                    <t-col
                      v-for="entry in visibleEntries"
                      :key="entry.path"
                      :xs="6"
                      :sm="4"
                      :md="4"
                      class="dashboard__entry-col"
                    >
                      <button class="dashboard__entry" type="button" @click="goTo(entry.path)">
                        <span class="dashboard__entry-icon"><t-icon :name="entry.icon" /></span>
                        <span class="dashboard__entry-title">{{ entry.title }}</span>
                      </button>
                    </t-col>
                  </t-row>
                  <t-empty v-else description="暂无可访问的模块" />
                </t-loading>
              </t-card>
            </t-col>
          </t-row>

          <!-- 第 3 行：两张趋势图并排 -->
          <t-row :gutter="[16, 16]" class="dashboard__row">
            <t-col class="dashboard__half-col">
              <t-card :title="loginTrendTitle" :bordered="false" class="dashboard__card">
                <t-loading :loading="isLoading" size="small">
                  <EChart v-if="!overviewError" :height="260" :option="loginTrendOption" />
                  <t-empty v-else description="暂无趋势数据" />
                </t-loading>
              </t-card>
            </t-col>

            <t-col class="dashboard__half-col">
              <t-card :title="latencyTrendTitle" :bordered="false" class="dashboard__card">
                <t-loading :loading="isLoading" size="small">
                  <EChart v-if="!overviewError" :height="260" :option="latencyTrendOption" />
                  <t-empty v-else description="暂无趋势数据" />
                </t-loading>
              </t-card>
            </t-col>
          </t-row>
        </t-col>

        <!--
          右栏：固定 360px。
          服务器状态卡仅对超级管理员渲染：非超管时 v-if 为 false，DOM 里不会出现该卡片，
          也不会发起 /api/business/monitor/server-status 请求（后端对非超管返回 403）。
        -->
        <t-col class="dashboard__aside">
          <t-row :gutter="[16, 16]">
            <t-col v-if="isSuperAdmin" class="dashboard__full-col">
              <t-card title="服务器状态" :bordered="false" class="dashboard__card">
                <template #actions>
                  <t-button variant="text" :loading="isStatusLoading" @click="loadServerStatus">
                    刷新
                  </t-button>
                </template>

                <t-alert v-if="statusError" theme="error" :message="statusError" />
                <template v-else>
                  <!-- 服务自检信息：沿用免鉴权 /health，保证采集接口异常时仍可判活 -->
                  <t-descriptions :column="1" size="small">
                    <t-descriptions-item label="服务状态">
                      <t-tag :theme="isHealthy ? 'success' : 'danger'" variant="light">
                        {{ health?.status ?? '-' }}
                      </t-tag>
                    </t-descriptions-item>
                    <t-descriptions-item label="应用名称">{{ health?.app ?? '-' }}</t-descriptions-item>
                    <t-descriptions-item label="运行 profile">{{ health?.profile ?? '-' }}</t-descriptions-item>
                    <t-descriptions-item label="服务端时间">{{ formattedTime }}</t-descriptions-item>
                  </t-descriptions>

                  <!-- CPU / 内存 / 磁盘：进度条 + 容量文本 -->
                  <div class="dashboard__usage-list">
                    <div v-for="item in usageItems" :key="item.key" class="dashboard__usage">
                      <div class="dashboard__usage-head">
                        <span class="dashboard__usage-label">{{ item.label }}</span>
                        <span class="dashboard__usage-value">{{ item.value }}</span>
                      </div>
                      <t-progress
                        :percentage="item.percent"
                        :label="false"
                        :status="item.status"
                        size="small"
                      />
                      <div class="dashboard__usage-sub">{{ item.sub }}</div>
                    </div>
                  </div>

                  <!-- 数据库 / Redis：只展示聚合数值，不含连接串与密码 -->
                  <t-descriptions :column="1" size="small">
                    <t-descriptions-item label="数据库">
                      <t-tag :theme="databaseStateTheme" variant="light">{{ databaseStateText }}</t-tag>
                      <span class="dashboard__usage-hint">{{ serverStatus?.database.name || '-' }}</span>
                    </t-descriptions-item>
                    <t-descriptions-item label="数据库大小">
                      {{ serverStatus?.database.size_text || '-' }}
                    </t-descriptions-item>
                    <t-descriptions-item label="数据库连接">
                      {{ serverStatus ? `${serverStatus.database.connections} 个` : '-' }}
                    </t-descriptions-item>
                    <t-descriptions-item label="Redis">
                      <t-tag :theme="redisStateTheme" variant="light">{{ redisStateText }}</t-tag>
                      <span class="dashboard__usage-hint">{{ serverStatus?.redis.used_memory_text || '-' }}</span>
                    </t-descriptions-item>
                    <t-descriptions-item label="Redis 客户端">
                      {{ serverStatus ? `${serverStatus.redis.connected_clients} 个` : '-' }}
                    </t-descriptions-item>
                    <t-descriptions-item label="Redis 运行">
                      {{ redisUptimeText }}
                    </t-descriptions-item>
                  </t-descriptions>

                  <div class="dashboard__status-foot">更新于 {{ statusUpdatedAt || '-' }}</div>
                </template>
              </t-card>
            </t-col>
          </t-row>
        </t-col>
      </t-row>
    </t-loading>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue';
import { useRouter } from 'vue-router';
import dayjs from 'dayjs';
import type { EChartsCoreOption } from 'echarts';
import EChart from '@/components/EChart.vue';
import { fetchHealth, type HealthRes } from '@/api/health';
import { fetchServerStatus, type ServerStatusRes } from '@/api/business/monitor-api';
import { fetchOverview, type OverviewRes } from '@/api/support/dashboard-api';
import { ApiError } from '@/api/request';
import { useUserStore } from '@/stores/user';

/** 工作台概览数据，加载中或未加载时为 null */
const overview = ref<OverviewRes | null>(null);
/** 健康检查数据 */
const health = ref<HealthRes | null>(null);
/** 概览加载中标记 */
const isLoading = ref(false);
/** 健康检查加载中标记 */
const isHealthLoading = ref(false);
/** 概览加载失败提示（不阻断页面渲染，图表降级为空图） */
const overviewError = ref('');
/** 健康检查失败提示 */
const healthError = ref('');

/** 服务器状态数据（仅超级管理员加载，未加载时为 null） */
const serverStatus = ref<ServerStatusRes | null>(null);
/** 服务器状态加载中标记 */
const isStatusLoading = ref(false);
/** 服务器状态加载失败提示 */
const statusError = ref('');
/** 服务器状态最近一次刷新时间（HH:mm:ss） */
const statusUpdatedAt = ref('');

/**
 * 首页只展示 8 个核心指标（后端共下发 12 个）。
 *
 * 挑选原则：覆盖「人（用户/角色）— 行为（登录/在线）— 内容量（菜单）— 运维（任务）— 审计（日志）」，
 * 数量再少就偏科、更多则一屏放不下（每行 4 个 × 2 行正好占满左栏首屏）。
 * 顺序即展示顺序；后端未下发的键会被自动跳过。
 *
 * 注：`avg_login_ms`（平均登录耗时）已移出——右侧「登录耗时趋势」图已表达该口径，
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
  // 后端调整了指标集合时兜底：不足 8 个就按原顺序补齐，避免首页出现空行
  return picked.length === METRIC_KEYS.length
    ? picked
    : [...picked, ...all.filter((m) => !METRIC_KEYS.includes(m.key as never))].slice(0, 8);
});

/** 快捷入口配置：path 与路由表一致，permission 与后端菜单权限码一致 */
interface QuickEntry {
  /** 入口标题 */
  title: string;
  /** 跳转路由路径 */
  path: string;
  /** 图标名（TDesign 图标库） */
  icon: string;
  /** 访问所需权限码 */
  permission: string;
}

const QUICK_ENTRIES: QuickEntry[] = [
  { title: '用户管理', path: '/system/user', icon: 'user', permission: 'system:user:list' },
  { title: '角色管理', path: '/system/role', icon: 'usergroup', permission: 'system:role:list' },
  { title: '菜单管理', path: '/system/menu', icon: 'menu', permission: 'system:menu:list' },
  { title: '部门管理', path: '/system/dept', icon: 'app', permission: 'system:dept:list' },
  { title: '字典管理', path: '/support/dict', icon: 'setting', permission: 'support:dict:list' },
  { title: '参数配置', path: '/support/config', icon: 'setting', permission: 'support:config:list' },
  { title: '文件管理', path: '/support/file', icon: 'folder', permission: 'support:file:list' },
  { title: '定时任务', path: '/support/job', icon: 'dashboard', permission: 'support:job:list' },
  { title: '在线用户', path: '/support/online', icon: 'user-time', permission: 'support:online:list' },
  { title: '操作日志', path: '/support/log', icon: 'lock-on', permission: 'support:log:list' },
];

const router = useRouter();
const userStore = useUserStore();

const isHealthy = computed(() => health.value?.status === 'ok');
const today = dayjs().format('YYYY年MM月DD日');

/** 按时段生成问候语 */
const greeting = computed(() => {
  const hour = dayjs().hour();
  if (hour < 6) return '凌晨好';
  if (hour < 12) return '上午好';
  if (hour < 14) return '中午好';
  if (hour < 18) return '下午好';
  return '晚上好';
});

const formattedTime = computed(() =>
  health.value ? dayjs(health.value.time).format('YYYY-MM-DD HH:mm:ss') : '-',
);

/** 趋势窗口天数：概览未就绪时按后端默认 14 天占位 */
const trendDays = computed(() => overview.value?.trend_days ?? 14);

/** 卡片标题必须走绑定：写在静态属性里的 {{ }} 不会被 Vue 插值，会原样显示 */
const loginTrendTitle = computed(() => `登录趋势（近 ${trendDays.value} 天）`);
const latencyTrendTitle = computed(() => `登录耗时趋势（近 ${trendDays.value} 天）`);

/** 按权限过滤后的快捷入口 */
const visibleEntries = computed(() =>
  QUICK_ENTRIES.filter((entry) => userStore.hasPermission(entry.permission)),
);

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
 * 避免一屏 12 张卡出现 6 种颜色造成视觉噪音。
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

function goTo(path: string): void {
  void router.push(path);
}

/** 拉取工作台概览：失败只提示，不阻塞其余卡片 */
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

/** 调用后端 /health，验证 Vite 代理、CORS 与统一响应体是否打通 */
async function loadHealth(): Promise<void> {
  isHealthLoading.value = true;
  healthError.value = '';
  try {
    health.value = await fetchHealth();
  } catch (error) {
    health.value = null;
    healthError.value = error instanceof ApiError ? error.message : '未知错误';
  } finally {
    isHealthLoading.value = false;
  }
}

/**
 * 是否超级管理员。
 *
 * 服务器状态（CPU / 内存 / 磁盘 / DB / Redis）只对超管开放：后端用
 * `business:monitor:server-status` 权限码（只授予 SUPER_ADMIN）硬校验，
 * 前端据此决定是否渲染整卡、是否发起请求。
 * 这里按**角色编码**判定而不是 `hasPermission`：超管持有 `*:*:*` 通配码，
 * 若将来把该权限点授予普通角色，`hasPermission` 会放行，与「仅超管可见」相悖。
 */
const isSuperAdmin = computed(() => userStore.roles.includes('SUPER_ADMIN'));

/** CPU / 内存 / 磁盘三条进度条的数据源 */
interface UsageItem {
  /** 稳定 key，用于 v-for */
  key: string;
  /** 指标名 */
  label: string;
  /** 百分比（0-100） */
  percent: number;
  /** 主数值文本 */
  value: string;
  /** 次要说明文本 */
  sub: string;
  /** 进度条状态色：低负载 success / 中负载 warning / 高负载 error */
  status: 'success' | 'warning' | 'error';
}

/** 进度条状态色：≥85% 红、≥70% 橙、其余绿 */
function usageStatus(percent: number): 'success' | 'warning' | 'error' {
  if (percent >= 85) return 'error';
  if (percent >= 70) return 'warning';
  return 'success';
}

/** 百分比文本：空值显示 `-`，其余保留 1 位小数 */
function percentText(percent?: number): string {
  return typeof percent === 'number' && Number.isFinite(percent) ? `${percent.toFixed(1)}%` : '-';
}

/** 三条用量进度条（未采集到时全部显示 `-`，不报错） */
const usageItems = computed<UsageItem[]>(() => {
  const status = serverStatus.value;
  if (!status) {
    return [
      { key: 'cpu', label: 'CPU 占用', percent: 0, value: '-', sub: '-', status: 'success' },
      { key: 'memory', label: '内存占用', percent: 0, value: '-', sub: '-', status: 'success' },
      { key: 'disk', label: '磁盘占用', percent: 0, value: '-', sub: '-', status: 'success' },
    ];
  }
  return [
    {
      key: 'cpu',
      label: 'CPU 占用',
      percent: status.cpu.percent,
      value: percentText(status.cpu.percent),
      sub: `${status.cpu.cores} 核逻辑核心`,
      status: usageStatus(status.cpu.percent),
    },
    {
      key: 'memory',
      label: '内存占用',
      percent: status.memory.percent,
      value: percentText(status.memory.percent),
      sub: `${status.memory.used_text || '-'} / ${status.memory.total_text || '-'}`,
      status: usageStatus(status.memory.percent),
    },
    {
      key: 'disk',
      label: '磁盘占用',
      percent: status.disk.percent,
      value: percentText(status.disk.percent),
      sub: `${status.disk.used_text || '-'} / ${status.disk.total_text || '-'}`,
      status: usageStatus(status.disk.percent),
    },
  ];
});

/** 数据库连通状态 -> TDesign 标签主题 */
const databaseStateTheme = computed(() =>
  serverStatus.value?.database.state === 'up' ? 'success' : 'danger',
);
/** 数据库连通状态 -> 展示文本 */
const databaseStateText = computed(() =>
  serverStatus.value?.database.state === 'up' ? '正常' : '异常',
);
/** Redis 连通状态 -> TDesign 标签主题 */
const redisStateTheme = computed(() =>
  serverStatus.value?.redis.state === 'up' ? 'success' : 'danger',
);
/** Redis 连通状态 -> 展示文本 */
const redisStateText = computed(() => (serverStatus.value?.redis.state === 'up' ? '正常' : '异常'));

/** Redis 运行时长：秒 -> 「X 天 Y 小时」/「Y 小时 Z 分」/「Z 分」 */
function formatUptime(seconds?: number): string {
  if (typeof seconds !== 'number' || !Number.isFinite(seconds) || seconds <= 0) return '-';
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) return `${days} 天 ${hours} 小时`;
  if (hours > 0) return `${hours} 小时 ${minutes} 分`;
  return `${minutes} 分`;
}

const redisUptimeText = computed(() => formatUptime(serverStatus.value?.redis.uptime_seconds));

/** 拉取服务器状态；非超管直接跳过（不发必然 403 的请求） */
async function loadServerStatus(): Promise<void> {
  if (!isSuperAdmin.value) return;
  isStatusLoading.value = true;
  statusError.value = '';
  try {
    serverStatus.value = await fetchServerStatus();
    statusUpdatedAt.value = dayjs().format('HH:mm:ss');
  } catch (error) {
    serverStatus.value = null;
    statusError.value = error instanceof ApiError ? error.message : '服务器状态加载失败';
  } finally {
    isStatusLoading.value = false;
  }
}

/** 轮询间隔：20s，兼顾实时性与后端 CPU 采样开销 */
const STATUS_POLL_INTERVAL = 20000;
/** 轮询句柄：页面不可见时暂停（切回来立即补一次） */
let statusTimer: ReturnType<typeof setInterval> | null = null;

/** 停止轮询（幂等） */
function stopStatusPolling(): void {
  if (statusTimer !== null) {
    clearInterval(statusTimer);
    statusTimer = null;
  }
}

/** 启动轮询：仅超管启用 */
function startStatusPolling(): void {
  stopStatusPolling();
  if (!isSuperAdmin.value) return;
  statusTimer = setInterval(() => {
    if (document.visibilityState === 'visible') {
      void loadServerStatus();
    }
  }, STATUS_POLL_INTERVAL);
}

/** 标签页重新可见时补一次刷新，避免长时间停留在旧数值上 */
function handleVisibilityChange(): void {
  if (document.visibilityState === 'visible') {
    void loadServerStatus();
  }
}

onMounted(() => {
  void loadOverview();
  void loadHealth();
  // 服务器状态仅超管可见：非超管既不请求也不轮询，DOM 中也不会出现该卡片
  if (isSuperAdmin.value) {
    void loadServerStatus();
    startStatusPolling();
    document.addEventListener('visibilitychange', handleVisibilityChange);
  }
});

onUnmounted(() => {
  stopStatusPolling();
  document.removeEventListener('visibilitychange', handleVisibilityChange);
});
</script>

<style scoped lang="less">
.dashboard {
  &__banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 24px 28px;
    color: var(--app-on-gradient, #1a2230);
    border-radius: 12px;
    background: var(--app-gradient-brand);
    box-shadow: 0 6px 20px rgb(111 138 184 / 24%);
  }

  &__greeting {
    margin: 0 0 6px;
    font-size: 20px;
    font-weight: 600;
    // 渐变右端是方案浅色，白色微光让深色文字在浅段边缘更清晰
    text-shadow: 0 1px 2px rgb(255 255 255 / 60%);
  }

  &__subtitle {
    margin: 0;
    font-size: 13px;
    // 渐变终点是浅色，副标题用同色文字（略透明以区分层级，主色端仍 ≥4.5）
    color: var(--app-on-gradient, #1a2230);
    opacity: 0.92;
    text-shadow: 0 1px 2px rgb(255 255 255 / 60%);
  }

  &__banner-icon {
    font-size: 72px;
    color: var(--app-on-gradient, #1a2230);
    opacity: 0.12;
  }

  // t-row 用 gutter 负边距（-8px）抵消列 padding，两栏基准宽度需按内容盒计算，
  // 否则 360px + 1664px 会超出容器而换行。这里给 body 显式 nowrap + 负边距归零。
  &__body {
    margin-top: 16px;
    flex-wrap: nowrap;

    @media (max-width: 1199px) {
      flex-wrap: wrap;
    }
  }

  // 左右两栏：右栏固定 360px，左栏自适应。
  // 宽度写在样式里而不是 t-col 的 flex prop —— prop 会被框架写成 inline style，
  // 媒体查询压不住。min-width:0 必需：flex item 默认 min-width:auto 会被内部宽内容撑开。
  &__main {
    flex: 1 1 auto;
    width: 0;
    min-width: 0;
  }

  &__aside {
    flex: 0 0 360px;
    width: 360px;
    min-width: 0;
  }

  // 窄屏（<1200px）：右侧栏落到左栏下方通栏
  @media (max-width: 1199px) {
    &__body {
      flex-wrap: wrap;

      .dashboard__main,
      .dashboard__aside {
        flex: 1 1 100%;
        width: 100%;
      }
    }
  }

  &__alert {
    margin-bottom: 16px;
  }

  &__row {
    margin-top: 16px;
  }

  // 整行列（快捷入口卡 / 右栏系统状态卡）：显式占满 100%。
  // 不依赖 t-col 的断点类名——12 列制里没有 t-col-xs-24，只写 :xs 会在部分屏宽失效，
  // 导致卡片按内容收缩、右侧留下空白。
  &__full-col {
    flex: 1 1 100%;
    width: 100%;
    min-width: 0;
    max-width: 100%;
  }

  // 半宽列（两张趋势图）：中屏及以上各占 50%，窄屏回落整行。
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

  // ---------- 服务器状态卡（右栏，仅超管） ----------
  &__usage-list {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin: 12px 0 4px;
  }

  &__usage {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }

  &__usage-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
    font-size: 13px;
  }

  &__usage-label {
    color: var(--td-text-color-secondary);
  }

  &__usage-value {
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__usage-sub {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__usage-hint {
    margin-left: 8px;
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }

  &__status-foot {
    margin-top: 8px;
    font-size: 12px;
    text-align: right;
    color: var(--td-text-color-placeholder);
  }


  // ---------- 指标卡片 ----------
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

  // ---------- 快捷入口 ----------
  // 5 不能被 12 整除，TDesign 没有「一行 5 个」的断点。方案：大屏用 flex-basis:20%
  // 强制每行 5 个（flex-grow 均分剩余空间，窄屏回落到 12 列制分栏）。
  // 注意不能用 flex:1 1 0 —— flex-basis:0 会让 10 个都挤进同一行。
  &__entries {
    @media (min-width: 1200px) {
      .dashboard__entry-col {
        flex: 1 1 20%;
        max-width: 20%;
      }
    }
  }

  &__entry-col {
    display: flex;
    // 栅格列只提供位置约束，按钮本身撑满该列
    max-width: 100%;
  }

  &__entry {
    display: flex;
    flex: 1;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    padding: 16px 12px;
    cursor: pointer;
    background-color: var(--td-bg-color-container-hover);
    border: 1px solid transparent;
    border-radius: 8px;
    transition: all 0.2s ease;

    &:hover {
      color: var(--td-brand-color);
      background-color: var(--td-brand-color-light);
      border-color: var(--td-brand-color-light-active);
      transform: translateY(-2px);
    }
  }

  &__entry-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 38px;
    height: 38px;
    font-size: 20px;
    color: var(--td-brand-color);
    background-color: var(--td-bg-color-container);
    border-radius: 10px;
  }

  &__entry-title {
    font-size: 14px;
    color: var(--td-text-color-primary);
    text-align: center;
  }
}
</style>

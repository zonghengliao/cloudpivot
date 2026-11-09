<template>
  <div class="server-status">
    <!-- ==================== 头部：标题 + 主机 + 总体状态 ==================== -->
    <header class="ss-hero">
      <div class="ss-hero__left">
        <span class="ss-hero__badge"><t-icon name="server" /></span>
        <div class="ss-hero__text">
          <h1 class="ss-hero__title">服务器状态</h1>
          <p class="ss-hero__sub">
            <template v-if="status">
              <span class="ss-hero__host">{{ status.host.hostname || '未知主机' }}</span>
              <span class="ss-hero__sep">·</span>
              <span>{{ status.host.os_name || '未知系统' }}</span>
              <span class="ss-hero__sep">·</span>
              <span>{{ status.host.arch }}</span>
            </template>
            <template v-else-if="loading">正在采集服务器指标…</template>
            <template v-else>暂无数据</template>
          </p>
        </div>
      </div>
      <div class="ss-hero__right">
        <span class="ss-pill" :class="`ss-pill--${overall.state}`">
          <i class="ss-pill__dot" />
          {{ overall.label }}
        </span>
        <span v-if="collectedText" class="ss-hero__time">采集于 {{ collectedText }}</span>
      </div>
    </header>

    <!-- 采集失败但手上还有旧数据：只提示不遮挡（旧数据带「已过期」标记） -->
    <t-alert
      v-if="errorMessage && status"
      class="ss-alert"
      theme="warning"
      :message="`本次刷新失败：${errorMessage}（下方为 ${updatedText} 的数据）`"
    />

    <!-- 首次采集就失败 / 无权限：整页错误态，不留白屏 -->
    <section v-if="blockingError" class="ss-card ss-empty">
      <t-icon class="ss-empty__icon" :name="blockingError.icon" />
      <p class="ss-empty__title">{{ blockingError.title }}</p>
      <p class="ss-empty__desc">{{ blockingError.desc }}</p>
      <t-button theme="primary" :loading="loading" @click="load(true)">
        <template #icon><t-icon name="refresh" /></template>
        重新采集
      </t-button>
    </section>

    <template v-else>
      <!-- ==================== ① 四张指标卡 ==================== -->
      <section class="ss-metrics">
        <template v-if="status">
          <article v-for="card in metricCards" :key="card.key" class="ss-card ss-metric">
            <div class="ss-metric__head">
              <span class="ss-metric__label">{{ card.label }}</span>
              <t-icon class="ss-metric__icon" :name="card.icon" />
            </div>

            <div class="ss-metric__body">
              <div class="ss-ring" :class="{ 'ss-ring--decorate': card.mode === 'decorate' }">
                <!--
                  自绘 SVG 环：0% 起点在 12 点方向（rotate(-90deg)），
                  dashoffset 由 CSS transition 800ms ease-out 驱动 ——
                  数值变化时不只是跳字，环也跟着走。
                -->
                <svg class="ss-ring__svg" viewBox="0 0 120 120" aria-hidden="true">
                  <circle class="ss-ring__track" cx="60" cy="60" :r="RING_RADIUS" />
                  <circle
                    v-if="card.mode === 'percent'"
                    class="ss-ring__value"
                    :class="`ss-ring__value--${card.tone}`"
                    cx="60"
                    cy="60"
                    :r="RING_RADIUS"
                    :style="{
                      strokeDasharray: `${RING_CIRCUMFERENCE}`,
                      strokeDashoffset: `${ringOffset(card.key)}`,
                    }"
                  />
                </svg>
                <div class="ss-ring__center">
                  <span class="ss-ring__num">{{ card.valueText }}</span>
                  <span v-if="card.unit" class="ss-ring__unit">{{ card.unit }}</span>
                </div>
              </div>
            </div>

            <p class="ss-metric__foot" :title="card.foot">{{ card.foot }}</p>
            <p class="ss-metric__extra" :title="card.extra">{{ card.extra }}</p>
          </article>
        </template>

        <!-- 首屏骨架：4 张同尺寸占位，避免数据回来时整页跳动 -->
        <template v-else>
          <article v-for="n in 4" :key="`sk-${n}`" class="ss-card ss-metric ss-metric--skeleton">
            <div class="ss-skeleton ss-skeleton--label" />
            <div class="ss-skeleton ss-skeleton--ring" />
            <div class="ss-skeleton ss-skeleton--line" />
          </article>
        </template>
      </section>

      <!-- ==================== ② 两张 24h 趋势图 ==================== -->
      <section class="ss-charts">
        <article class="ss-card ss-chart">
          <div class="ss-chart__head">
            <div>
              <h2 class="ss-chart__title">CPU / 内存</h2>
              <p class="ss-chart__sub">近 24 小时 · 每 30 秒采样一次</p>
            </div>
            <span class="ss-chart__badge">{{ samples.length }} 个采样点</span>
          </div>

          <EChart v-if="hasTrend" :option="cpuMemOption" :height="228" />
          <div v-else class="ss-chart__empty">
            <t-icon name="chart-line" />
            <p>{{ trendEmptyText }}</p>
          </div>
        </article>

        <article class="ss-card ss-chart">
          <div class="ss-chart__head">
            <div>
              <h2 class="ss-chart__title">数据库 / Redis 连接</h2>
              <p class="ss-chart__sub">近 24 小时 · 连接数（个）</p>
            </div>
            <span class="ss-chart__badge">{{ samples.length }} 个采样点</span>
          </div>

          <EChart v-if="hasTrend" :option="connOption" :height="228" />
          <div v-else class="ss-chart__empty">
            <t-icon name="chart-line" />
            <p>{{ trendEmptyText }}</p>
          </div>
        </article>
      </section>

      <!-- ==================== ③ 服务健康清单 ==================== -->
      <section class="ss-card ss-health">
        <div class="ss-section__head">
          <div>
            <h2 class="ss-section__title">服务健康</h2>
            <p class="ss-section__sub">每 30 秒随页面一起探测；「未安装」是正常的部署状态</p>
          </div>
          <span class="ss-section__count">{{ healthyCount }} / {{ healthRows.length }} 正常</span>
        </div>

        <div class="ss-health__table" role="table">
          <div class="ss-health__row ss-health__row--head" role="row">
            <span role="columnheader">服务</span>
            <span role="columnheader">状态</span>
            <span role="columnheader">响应时间</span>
            <span role="columnheader">最后检查</span>
            <span role="columnheader">说明</span>
          </div>
          <div
            v-for="row in healthRows"
            :key="row.key"
            class="ss-health__row"
            :class="`ss-health__row--${row.state}`"
            role="row"
          >
            <span class="ss-health__name" role="cell">
              <i class="ss-health__dot" />
              {{ row.label }}
            </span>
            <span role="cell">
              <span class="ss-tag" :class="`ss-tag--${row.state}`">{{ row.stateText }}</span>
            </span>
            <span class="ss-health__mono" role="cell">{{ row.latencyText }}</span>
            <span class="ss-health__mono" role="cell">{{ row.checkedText }}</span>
            <span class="ss-health__detail" role="cell" :title="row.detail">{{ row.detail }}</span>
          </div>
        </div>
      </section>

      <!-- ==================== ④ 环境信息（2×2） ==================== -->
      <section class="ss-env">
        <article v-for="block in envBlocks" :key="block.key" class="ss-card ss-env__card">
          <div class="ss-env__head">
            <t-icon class="ss-env__icon" :name="block.icon" />
            <h2 class="ss-env__title">{{ block.title }}</h2>
          </div>
          <dl class="ss-env__list">
            <div v-for="item in block.items" :key="item.label" class="ss-env__item">
              <dt>{{ item.label }}</dt>
              <dd :class="{ 'ss-env__value--mono': item.mono }" :title="item.value">{{ item.value }}</dd>
            </div>
          </dl>
        </article>
      </section>

      <!-- ==================== ⑤ 底部操作栏（吸底） ==================== -->
      <footer class="ss-actions">
        <div class="ss-actions__buttons">
          <t-button theme="primary" :loading="loading" @click="load(true)">
            <template #icon><t-icon name="refresh" /></template>
            刷新
          </t-button>
          <t-button theme="default" variant="outline" :loading="downloading" @click="onDownload">
            <template #icon><t-icon name="download" /></template>
            下载诊断包
          </t-button>
        </div>
        <p class="ss-actions__hint">
          最后更新：{{ updatedText || '—' }}
          <span class="ss-actions__sep">·</span>
          {{ REFRESH_SECONDS }} 秒后自动刷新（{{ countdown }}s）
        </p>
      </footer>
    </template>
  </div>
</template>

<script setup lang="ts">
/**
 * 服务器状态（系统管理 → 服务器状态，仅超管）。
 *
 * # 数据来源（**全部真实**，没有前端编造的数字）
 *
 * | 区块 | 来源 |
 * |------|------|
 * | 四张指标卡 / 数据库 / Redis / 主机 / 进程 / 业务量 / Redis 延迟 | `GET /api/business/monitor/server-status` |
 * | 后端 API 响应时间 | 浏览器直接 `fetch('/health')` 计时（真实往返，不拿采集接口的耗时冒充） |
 * | 前端静态资源 | 浏览器 `HEAD index.html` 计时 |
 * | Hermes Agent | 后端 TCP 探测 `127.0.0.1:8100`；未监听 = 「未安装」（正常部署状态） |
 * | 两张趋势图 | **本地采样**：每次刷新把一行样本写进 localStorage，保留 24 小时 |
 *
 * # 为什么趋势图是前端本地采样
 *
 * 后端目前没有「历史指标表」（只有现场采集）。为一张运维页新增时序表 + 写放大不划算，
 * 因此由页面自己滚动采样：打开页面期间每 30 秒一个点，关掉页面就停。
 * 刷新页面不会丢（存在 localStorage），但**换浏览器 / 清缓存会丢** —— 这是可接受的
 * 代价，故图上明确标注「打开页面后开始采样」，不假装是服务端历史。
 *
 * # 视觉
 *
 * 配色全部取主题变量（`--td-*` / `--app-*`），不硬编码颜色：换配色方案 / 明暗模式时
 * 环、折线、状态灯、卡片一起变。环形进度用 CSS transition 800ms 从 0 长到实际值；
 * 卡片 hover 上浮 2px + 阴影加深（200ms）。
 */
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import { downloadDiagnosticBundle, fetchServerStatus, type ServerStatusRes } from '@/api/business/monitor-api';
import EChart from '@/components/EChart.vue';
import { usePalette } from '@/composables/usePalette';
import { saveBlob } from '@/utils/download';

// ==================== 常量 ====================

/** 自动刷新间隔（秒） */
const REFRESH_SECONDS = 30;
/** 本地采样保留窗口（毫秒） */
const SAMPLE_WINDOW_MS = 24 * 60 * 60 * 1000;
/** 采样点上限（24h / 30s = 2880，留一点余量） */
const SAMPLE_MAX = 2900;
/** localStorage 键（带版本号：字段结构变化时直接换键，不让旧数据把页面打崩） */
const SAMPLE_KEY = 'cloudpivot-server-status-samples-v1';

/** 环形几何：半径 52 → 周长 2πr（dasharray / dashoffset 都用它） */
const RING_RADIUS = 52;
const RING_CIRCUMFERENCE = 2 * Math.PI * RING_RADIUS;

/** 阈值：< 60 主题色 / 60-80 琥珀 / > 80 红（与卡面设计一致） */
const WARN_PERCENT = 60;
const DANGER_PERCENT = 80;

// ==================== 状态 ====================

/** 最近一次成功的采集结果 */
const status = ref<ServerStatusRes | null>(null);
/** 是否在请求中（首次进入与手动刷新都会置位） */
const loading = ref(false);
/** 是否在下载诊断包 */
const downloading = ref(false);
/** 本次刷新失败的原因（有旧数据时只提示，不清空） */
const errorMessage = ref('');
/** 首次采集失败的类型：403 与网络错误要给不同文案 */
const errorKind = ref<'none' | 'forbidden' | 'failed'>('none');

/** 后端 API 探测（/health 往返） */
const apiProbe = ref<{ state: 'ok' | 'error'; latency: number | null; checkedAt: string; detail: string } | null>(null);
/** 前端静态资源探测（index.html） */
const staticProbe = ref<{ state: 'ok' | 'error'; latency: number | null; checkedAt: string; detail: string } | null>(null);

/** 是否已完成首帧（用于环形从 0 长出来，而不是挂载即满） */
const ready = ref(false);
/** 距下次自动刷新的秒数 */
const countdown = ref(REFRESH_SECONDS);

/** 环形当前值（动画中间态；key 与卡片一一对应） */
const animated = reactive<Record<string, number>>({ cpu: 0, memory: 0, disk: 0 });

/** 采样点 */
interface Sample {
  /** 采样时间戳（毫秒） */
  t: number;
  /** CPU 使用率 */
  cpu: number;
  /** 内存使用率 */
  mem: number;
  /** 数据库连接数 */
  db: number;
  /** Redis 客户端数 */
  redis: number;
}
const samples = ref<Sample[]>([]);

/** 图表文字/线条色跟随主题（明暗切换时重算，避免深色模式下轴标签糊掉） */
const { version: paletteVersion } = usePalette();

// ==================== 小工具 ====================

/** 读取主题 CSS 变量（带兜底值，SSR / 首帧取不到时也不出空色） */
function cssVar(name: string, fallback: string): string {
  if (typeof window === 'undefined') return fallback;
  const value = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return value || fallback;
}

/** 把 `#rgb` / `#rrggbb` / `rgb(...)` 转成带透明度的颜色（渐变填充用） */
function withAlpha(color: string, alpha: number): string {
  const value = color.trim();
  if (value.startsWith('#')) {
    let hex = value.slice(1);
    if (hex.length === 3) {
      hex = hex
        .split('')
        .map((c) => c + c)
        .join('');
    }
    const num = Number.parseInt(hex, 16);
    if (Number.isNaN(num)) return value;
    const r = (num >> 16) & 0xff;
    const g = (num >> 8) & 0xff;
    const b = num & 0xff;
    return `rgba(${r}, ${g}, ${b}, ${alpha})`;
  }
  if (value.startsWith('rgba')) return value;
  if (value.startsWith('rgb(')) return `${value.slice(0, -1).replace('rgb(', 'rgba(')}, ${alpha})`;
  return value;
}

/** 秒 → 「12 天 6 小时」/「3 小时 12 分」/「8 分 12 秒」 */
function formatDuration(totalSeconds: number): string {
  const seconds = Math.max(0, Math.floor(totalSeconds || 0));
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) return `${days} 天 ${hours} 小时`;
  if (hours > 0) return `${hours} 小时 ${minutes} 分`;
  if (minutes > 0) return `${minutes} 分 ${seconds % 60} 秒`;
  return `${seconds} 秒`;
}

/** 秒 → 拆成「大数字 + 单位」（指标卡里 32px 数字只放数字本身） */
function splitDuration(totalSeconds: number): { value: string; unit: string } {
  const seconds = Math.max(0, Math.floor(totalSeconds || 0));
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) return { value: String(days), unit: `天 ${hours} 小时` };
  if (hours > 0) return { value: String(hours), unit: `小时 ${minutes} 分` };
  return { value: String(minutes), unit: `分 ${seconds % 60} 秒` };
}

/** 字节 → `8.0 GB` / `512 MB`（与后端 `format_bytes` 同口径，前端只用于二次格式化） */
function formatBytes(bytes: number): string {
  const value = Number(bytes) || 0;
  const gb = 1024 ** 3;
  const mb = 1024 ** 2;
  if (value >= gb) return `${(value / gb).toFixed(1)} GB`;
  return `${Math.round(value / mb)} MB`;
}

/** RFC3339 / ISO → `11-01 14:00`（本地时区） */
function formatDateTime(input?: string): string {
  if (!input) return '—';
  const date = new Date(input);
  if (Number.isNaN(date.getTime())) return '—';
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** 任意时间 → `14:30:22` */
function formatClock(input?: string | number): string {
  if (input === undefined || input === null || input === '') return '—';
  const date = new Date(input);
  if (Number.isNaN(date.getTime())) return '—';
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

/** 毫秒 → `12 ms` / `1.2 s` */
function formatLatency(ms?: number | null): string {
  if (ms === null || ms === undefined || Number.isNaN(ms)) return '—';
  if (ms >= 1000) return `${(ms / 1000).toFixed(1)} s`;
  return `${ms < 10 ? ms.toFixed(1) : Math.round(ms)} ms`;
}

/** 百分比取值 → 三档色（主题色 / 琥珀 / 红） */
function toneOf(percent: number): 'ok' | 'warn' | 'danger' {
  if (percent >= DANGER_PERCENT) return 'danger';
  if (percent >= WARN_PERCENT) return 'warn';
  return 'ok';
}

// ==================== 环形动画 ====================

/** 尊重「减少动态效果」系统偏好：关闭动画时直接落到目标值 */
function prefersReducedMotion(): boolean {
  return typeof window !== 'undefined' && window.matchMedia?.('(prefers-reduced-motion: reduce)').matches === true;
}

let rafId = 0;

/** 把三个环从当前值平滑推到目标值（600ms，easeOutCubic；数字与环同步） */
function animateRings(target: Record<string, number>): void {
  if (typeof window === 'undefined') return;
  window.cancelAnimationFrame(rafId);
  if (prefersReducedMotion()) {
    Object.assign(animated, target);
    return;
  }
  const from = { ...animated };
  const startedAt = performance.now();
  const duration = 800;
  const step = (now: number) => {
    const progress = Math.min(1, (now - startedAt) / duration);
    const eased = 1 - (1 - progress) ** 3;
    for (const key of Object.keys(target)) {
      const start = Number(from[key] ?? 0);
      animated[key] = start + (target[key] - start) * eased;
    }
    if (progress < 1) rafId = window.requestAnimationFrame(step);
  };
  rafId = window.requestAnimationFrame(step);
}

/** 某张卡的 dashoffset（0% → 整圈空，100% → 无偏移） */
function ringOffset(key: string): number {
  const percent = Math.min(100, Math.max(0, Number(animated[key] ?? 0)));
  return RING_CIRCUMFERENCE * (1 - percent / 100);
}

// ==================== 指标卡 ====================

interface MetricCard {
  /** 唯一键（与 `animated` 对齐） */
  key: string;
  /** 标签 */
  label: string;
  /** TDesign 图标名（必须真实存在） */
  icon: string;
  /** 环里的主数字 */
  valueText: string;
  /** 单位 */
  unit: string;
  /** 环下的第一行说明 */
  foot: string;
  /** 环下的第二行说明 */
  extra: string;
  /** 环的类型：百分比 / 装饰（运行时长没有百分比可画） */
  mode: 'percent' | 'decorate';
  /** 三档色 */
  tone: 'ok' | 'warn' | 'danger';
}

const metricCards = computed<MetricCard[]>(() => {
  const data = status.value;
  if (!data) return [];

  const cpu = Number(data.cpu.percent) || 0;
  const mem = Number(data.memory.percent) || 0;
  const disk = Number(data.disk.percent) || 0;
  const cores = Number(data.cpu.cores) || 0;
  const mhz = Number(data.host?.cpu_mhz) || 0;
  const uptime = splitDuration(Number(data.app?.uptime_seconds) || 0);

  const cpuFoot = mhz > 0 ? `${cores} 核 · ${(mhz / 1000).toFixed(1)} GHz` : `${cores} 核`;
  const load = data.host
    ? `5 分钟负载 ${Number(data.host.load5 || 0).toFixed(2)}`
    : '—';

  return [
    {
      key: 'cpu',
      label: 'CPU',
      icon: 'cpu',
      valueText: String(Math.round(animated.cpu ?? cpu)),
      unit: '%',
      foot: cpuFoot,
      extra: load,
      mode: 'percent',
      tone: toneOf(cpu),
    },
    {
      key: 'memory',
      label: '内存',
      icon: 'layers',
      valueText: String(Math.round(animated.memory ?? mem)),
      unit: '%',
      foot: `${formatBytes(data.memory.used_bytes)} / ${formatBytes(data.memory.total_bytes)}`,
      extra: `可用 ${formatBytes(Math.max(0, data.memory.total_bytes - data.memory.used_bytes))}`,
      mode: 'percent',
      tone: toneOf(mem),
    },
    {
      key: 'disk',
      label: '磁盘',
      icon: 'chart-pie',
      valueText: String(Math.round(animated.disk ?? disk)),
      unit: '%',
      foot: `${formatBytes(data.disk.used_bytes)} / ${formatBytes(data.disk.total_bytes)}`,
      extra: '根分区 /',
      mode: 'percent',
      tone: toneOf(disk),
    },
    {
      key: 'uptime',
      label: '运行时长',
      icon: 'time',
      valueText: uptime.value,
      unit: uptime.unit,
      foot: `启动 ${formatDateTime(data.app?.started_at)}`,
      extra: `主机已运行 ${formatDuration(Number(data.host?.uptime_seconds) || 0)}`,
      mode: 'decorate',
      tone: 'ok',
    },
  ];
});

// ==================== 趋势图 ====================

const hasTrend = computed(() => samples.value.length >= 2);

const trendEmptyText = computed(() => {
  if (samples.value.length === 0) {
    return '打开本页后开始采样：每 30 秒一个点，连续 1 分钟后即可成线';
  }
  return `已采集 ${samples.value.length} 个点，再等 ${Math.max(1, 2 - samples.value.length)} 个点即可成线`;
});

const chartColors = computed(() => {
  // 订阅配色版本号：切换方案 / 明暗时重算（否则图会停在旧色）
  void paletteVersion.value;
  return {
    cpu: cssVar('--td-chart-color-1', '#6f8ab8'),
    memory: cssVar('--td-chart-color-2', '#c1ac7b'),
    db: cssVar('--td-chart-color-1', '#6f8ab8'),
    redis: cssVar('--td-chart-color-4', '#a5b3cb'),
  };
});

/** 折线渐隐填充（顶部 26% 不透明 → 底部全透明） */
function areaGradient(color: string): Record<string, unknown> {
  return {
    type: 'linear',
    x: 0,
    y: 0,
    x2: 0,
    y2: 1,
    colorStops: [
      { offset: 0, color: withAlpha(color, 0.26) },
      { offset: 1, color: withAlpha(color, 0.02) },
    ],
  };
}

const trendLabels = computed(() =>
  samples.value.map((sample) => {
    const date = new Date(sample.t);
    return `${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}`;
  }),
);

/** 公共坐标轴/提示配置（两张图共用，减少重复） */
function baseAxis(): Record<string, unknown> {
  return {
    grid: { left: 6, right: 14, top: 34, bottom: 2, containLabel: true },
    tooltip: {
      trigger: 'axis',
      axisPointer: { type: 'line', lineStyle: { color: cssVar('--td-component-stroke', '#dce2e6') } },
      backgroundColor: cssVar('--td-bg-color-container', '#ffffff'),
      borderColor: cssVar('--td-component-stroke', '#dce2e6'),
      textStyle: { color: cssVar('--td-text-color-primary', '#2f3a4d'), fontSize: 12 },
    },
    legend: {
      top: 0,
      right: 0,
      itemWidth: 10,
      itemHeight: 10,
      icon: 'roundRect',
      textStyle: { color: cssVar('--td-text-color-secondary', '#5a6377'), fontSize: 12 },
    },
  };
}

const cpuMemOption = computed<Record<string, unknown>>(() => {
  const colors = chartColors.value;
  return {
    ...baseAxis(),
    xAxis: {
      type: 'category',
      boundaryGap: false,
      data: trendLabels.value,
      axisTick: { show: false },
      axisLabel: { color: cssVar('--td-text-color-secondary', '#5a6377'), fontSize: 11, hideOverlap: true },
      axisLine: { lineStyle: { color: cssVar('--td-component-stroke', '#dce2e6') } },
    },
    yAxis: {
      type: 'value',
      min: 0,
      max: 100,
      // 固定 25% 一档：`splitNumber: 4` 会被 echarts 取整成 0/30/60/90/100，读数很别扭
      interval: 25,
      axisLabel: { color: cssVar('--td-text-color-secondary', '#5a6377'), fontSize: 11, formatter: '{value}%' },
      splitLine: { lineStyle: { color: cssVar('--td-component-stroke', '#dce2e6'), type: 'dashed' } },
    },
    series: [
      {
        name: 'CPU',
        type: 'line',
        smooth: true,
        showSymbol: false,
        symbolSize: 6,
        data: samples.value.map((sample) => Number(sample.cpu.toFixed(1))),
        lineStyle: { width: 2, color: colors.cpu },
        itemStyle: { color: colors.cpu },
        areaStyle: { color: areaGradient(colors.cpu) },
        emphasis: { focus: 'series' },
      },
      {
        name: '内存',
        type: 'line',
        smooth: true,
        showSymbol: false,
        symbolSize: 6,
        data: samples.value.map((sample) => Number(sample.mem.toFixed(1))),
        lineStyle: { width: 2, color: colors.memory },
        itemStyle: { color: colors.memory },
        areaStyle: { color: areaGradient(colors.memory) },
        emphasis: { focus: 'series' },
      },
    ],
  };
});

const connOption = computed<Record<string, unknown>>(() => {
  const colors = chartColors.value;
  return {
    ...baseAxis(),
    xAxis: {
      type: 'category',
      boundaryGap: false,
      data: trendLabels.value,
      axisTick: { show: false },
      axisLabel: { color: cssVar('--td-text-color-secondary', '#5a6377'), fontSize: 11, hideOverlap: true },
      axisLine: { lineStyle: { color: cssVar('--td-component-stroke', '#dce2e6') } },
    },
    yAxis: {
      type: 'value',
      minInterval: 1,
      axisLabel: { color: cssVar('--td-text-color-secondary', '#5a6377'), fontSize: 11 },
      splitLine: { lineStyle: { color: cssVar('--td-component-stroke', '#dce2e6'), type: 'dashed' } },
    },
    series: [
      {
        name: 'DB 连接',
        type: 'line',
        smooth: true,
        showSymbol: false,
        data: samples.value.map((sample) => sample.db),
        lineStyle: { width: 2, color: colors.db },
        itemStyle: { color: colors.db },
        areaStyle: { color: areaGradient(colors.db) },
        emphasis: { focus: 'series' },
      },
      {
        name: 'Redis 客户端',
        type: 'line',
        smooth: true,
        showSymbol: false,
        data: samples.value.map((sample) => sample.redis),
        lineStyle: { width: 2, color: colors.redis, type: 'dashed' },
        itemStyle: { color: colors.redis },
        areaStyle: { color: areaGradient(colors.redis) },
        emphasis: { focus: 'series' },
      },
    ],
  };
});

// ==================== 服务健康 ====================

type HealthState = 'ok' | 'warning' | 'error' | 'muted';

interface HealthRow {
  key: string;
  label: string;
  state: HealthState;
  stateText: string;
  latencyText: string;
  checkedText: string;
  detail: string;
}

const STATE_TEXT: Record<HealthState, string> = {
  ok: '正常',
  warning: '警告',
  error: '异常',
  muted: '未安装',
};

const healthRows = computed<HealthRow[]>(() => {
  const data = status.value;
  const nowText = formatClock(data?.collected_at);

  const apiRow: HealthRow = {
    key: 'api',
    label: '后端 API',
    state: apiProbe.value?.state === 'ok' ? 'ok' : apiProbe.value ? 'error' : 'muted',
    stateText: apiProbe.value?.state === 'ok' ? '正常' : apiProbe.value ? '异常' : '未探测',
    latencyText: formatLatency(apiProbe.value?.latency ?? null),
    checkedText: formatClock(apiProbe.value?.checkedAt),
    detail: apiProbe.value?.detail ?? '尚未探测（等待首帧）',
  };

  const dbState = data?.database.state === 'up' ? 'ok' : 'error';
  const dbRow: HealthRow = {
    key: 'postgres',
    label: 'PostgreSQL',
    state: data ? dbState : 'muted',
    stateText: data ? STATE_TEXT[dbState] : '未知',
    latencyText: formatLatency(data?.database.latency_ms ?? null),
    checkedText: nowText,
    detail: data
      ? data.database.state === 'up'
        ? `PostgreSQL ${data.database.server_version || '?'} · ${data.database.connections} 个连接`
        : '数据库不可用（详见诊断包 health.json）'
      : '—',
  };

  // Redis 可能整个模块未启用（`[redis] enabled = false`）：那是「未启用」而不是「异常」
  const redisDown = data?.redis.state !== 'up';
  const redisRow: HealthRow = {
    key: 'redis',
    label: 'Redis',
    state: !data ? 'muted' : redisDown ? 'muted' : 'ok',
    stateText: !data ? '未知' : redisDown ? '未启用' : '正常',
    latencyText: formatLatency(data?.redis.latency_ms ?? null),
    checkedText: nowText,
    detail: data
      ? redisDown
        ? '缓存未启用（[redis] enabled = false）或不可达'
        : `Redis ${data.redis.version || '?'} · ${data.redis.connected_clients} 个客户端`
      : '—',
  };

  const hermesUp = data?.hermes.state === 'up';
  const hermesRow: HealthRow = {
    key: 'hermes',
    label: 'Hermes Agent',
    state: !data ? 'muted' : hermesUp ? 'ok' : 'muted',
    stateText: !data ? '未知' : hermesUp ? '正常' : '未安装',
    latencyText: formatLatency(data?.hermes.latency_ms ?? null),
    checkedText: nowText,
    detail: data?.hermes.detail ?? '未探测',
  };

  const staticRow: HealthRow = {
    key: 'static',
    label: '前端静态资源',
    state: staticProbe.value?.state === 'ok' ? 'ok' : staticProbe.value ? 'error' : 'muted',
    stateText: staticProbe.value?.state === 'ok' ? '正常' : staticProbe.value ? '异常' : '未探测',
    latencyText: formatLatency(staticProbe.value?.latency ?? null),
    checkedText: formatClock(staticProbe.value?.checkedAt),
    detail: staticProbe.value?.detail ?? '尚未探测（等待首帧）',
  };

  const diskPercent = Number(data?.disk.percent) || 0;
  const diskTone = toneOf(diskPercent);
  const diskState: HealthState = !data ? 'muted' : diskTone === 'danger' ? 'error' : diskTone === 'warn' ? 'warning' : 'ok';
  const diskRow: HealthRow = {
    key: 'disk',
    label: '磁盘空间',
    state: diskState,
    stateText: data ? STATE_TEXT[diskState] : '未知',
    latencyText: data ? `${diskPercent.toFixed(1)}%` : '—',
    checkedText: nowText,
    detail: data
      ? `根分区 ${formatBytes(data.disk.used_bytes)} / ${formatBytes(data.disk.total_bytes)}（≥${WARN_PERCENT}% 警告、≥${DANGER_PERCENT}% 异常）`
      : '—',
  };

  return [apiRow, dbRow, redisRow, hermesRow, staticRow, diskRow];
});

const healthyCount = computed(() => healthRows.value.filter((row) => row.state === 'ok').length);

const overall = computed<{ state: HealthState | 'warning'; label: string }>(() => {
  const rows = healthRows.value;
  if (rows.some((row) => row.state === 'error')) return { state: 'error', label: '存在异常' };
  if (rows.some((row) => row.state === 'warning')) return { state: 'warning', label: '需要关注' };
  if (!status.value) return { state: 'muted', label: '暂无数据' };
  return { state: 'ok', label: '运行正常' };
});

// ==================== 环境信息 ====================

interface EnvItem {
  label: string;
  value: string;
  mono?: boolean;
}

const envBlocks = computed<Array<{ key: string; title: string; icon: string; items: EnvItem[] }>>(() => {
  const data = status.value;
  if (!data) return [];
  return [
    {
      key: 'app',
      title: '应用',
      icon: 'app',
      items: [
        { label: '版本', value: `v${data.app.version || '—'}` },
        { label: 'commit', value: data.app.commit || 'unknown', mono: true },
        { label: 'profile', value: data.app.profile || '—', mono: true },
        { label: '启动', value: formatDateTime(data.app.started_at) },
      ],
    },
    {
      key: 'db',
      title: '数据库',
      icon: 'data-base',
      items: [
        { label: '版本', value: data.database.server_version || '—' },
        { label: '迁移', value: `${data.database.migrations} 条` },
        { label: '库名', value: data.database.name || '—', mono: true },
        { label: '大小', value: data.database.size_text || '—' },
      ],
    },
    {
      key: 'runtime',
      title: '运行时',
      icon: 'cpu',
      items: [
        { label: '主机', value: data.host.hostname || '—', mono: true },
        { label: '内核', value: data.host.kernel || '—', mono: true },
        { label: 'Rust', value: data.app.rust_version || 'unknown', mono: true },
        { label: 'Redis', value: data.redis.state === 'up' ? data.redis.version || '—' : '未启用' },
      ],
    },
    {
      key: 'business',
      title: '业务',
      icon: 'chart',
      items: [
        { label: '用户', value: `${data.business.users_total} 个` },
        { label: '在线会话', value: `${data.business.online_users} 个` },
        { label: '今日登录', value: `${data.business.login_today} 次` },
        { label: '今日操作', value: `${data.business.op_log_today} 次` },
      ],
    },
  ];
});

// ==================== 提示文案 ====================

const collectedText = computed(() => (status.value ? formatClock(status.value.collected_at) : ''));
const updatedText = computed(() => (status.value ? formatClock(status.value.collected_at) : ''));

const blockingError = computed<{ icon: string; title: string; desc: string } | null>(() => {
  if (errorKind.value === 'forbidden') {
    return {
      icon: 'lock-on',
      title: '无权访问',
      desc: '服务器状态仅超级管理员可见（系统会把结果写进操作日志）。',
    };
  }
  if (errorKind.value === 'failed' && !status.value) {
    return {
      icon: 'error-circle',
      title: '采集失败',
      desc: errorMessage.value || '无法连接后端，请确认服务是否在运行。',
    };
  }
  return null;
});

// ==================== 采样存储 ====================

/** 从 localStorage 读采样（脏数据一律丢弃，绝不让它把页面打崩） */
function loadSamples(): void {
  try {
    const raw = localStorage.getItem(SAMPLE_KEY);
    if (!raw) return;
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return;
    const cutoff = Date.now() - SAMPLE_WINDOW_MS;
    const clean = parsed
      .filter((item): item is Sample => {
        if (!item || typeof item !== 'object') return false;
        const row = item as Partial<Sample>;
        return (
          typeof row.t === 'number' &&
          typeof row.cpu === 'number' &&
          typeof row.mem === 'number' &&
          typeof row.db === 'number' &&
          typeof row.redis === 'number' &&
          row.t > cutoff
        );
      })
      .slice(-SAMPLE_MAX);
    samples.value = clean;
  } catch {
    // 解析失败（旧格式 / 被手动改坏）→ 从空开始，不报错
    localStorage.removeItem(SAMPLE_KEY);
  }
}

/** 追加一个采样点并落盘（同一秒内刷新不重复写点） */
function pushSample(data: ServerStatusRes): void {
  const now = Date.now();
  const last = samples.value[samples.value.length - 1];
  const next: Sample = {
    t: now,
    cpu: Number(data.cpu.percent) || 0,
    mem: Number(data.memory.percent) || 0,
    db: Number(data.database.connections) || 0,
    redis: Number(data.redis.connected_clients) || 0,
  };
  // 30 秒粒度：手动连点「刷新」时不要把曲线打成毛刺
  if (last && now - last.t < 15_000) {
    samples.value = [...samples.value.slice(0, -1), next];
  } else {
    samples.value = [...samples.value, next];
  }
  const cutoff = now - SAMPLE_WINDOW_MS;
  samples.value = samples.value.filter((sample) => sample.t > cutoff).slice(-SAMPLE_MAX);
  try {
    localStorage.setItem(SAMPLE_KEY, JSON.stringify(samples.value));
  } catch {
    // 无痕模式 / 配额满：仅丢历史采样，页面其余功能不受影响
  }
}

// ==================== 探测 ====================

/** 后端 API 响应时间：浏览器直接打一次 `/health`（免认证、不查库，最接近纯网络往返） */
async function probeBackendApi(): Promise<void> {
  const started = performance.now();
  try {
    const response = await fetch(`${window.location.origin}/health`, { cache: 'no-store' });
    const latency = performance.now() - started;
    apiProbe.value = {
      state: response.ok ? 'ok' : 'error',
      latency,
      checkedAt: new Date().toISOString(),
      detail: `GET /health → HTTP ${response.status}`,
    };
  } catch {
    apiProbe.value = {
      state: 'error',
      latency: null,
      checkedAt: new Date().toISOString(),
      detail: 'GET /health 不可达（后端进程可能已停止）',
    };
  }
}

/** 前端静态资源响应时间：HEAD 当前部署的 index.html（真实浏览器往返） */
async function probeStatic(): Promise<void> {
  const started = performance.now();
  try {
    const url = new URL('index.html', `${window.location.origin}${import.meta.env.BASE_URL || '/'}`);
    url.searchParams.set('_probe', String(Date.now()));
    const response = await fetch(url.toString(), { method: 'HEAD', cache: 'no-store' });
    const latency = performance.now() - started;
    staticProbe.value = {
      state: response.ok ? 'ok' : 'error',
      latency,
      checkedAt: new Date().toISOString(),
      detail: response.ok ? 'index.html 可访问' : `index.html → HTTP ${response.status}`,
    };
  } catch {
    staticProbe.value = {
      state: 'error',
      latency: null,
      checkedAt: new Date().toISOString(),
      detail: '静态资源不可访问（前端容器 / nginx 异常）',
    };
  }
}

// ==================== 加载 ====================

/** 是否正在执行自动刷新（用于切换刷新图标态，不影响骨架） */
const refreshing = ref(false);

/**
 * 拉一次全量状态。
 *
 * @param manual 是否手动触发（手动失败要弹 toast；自动刷新失败只在页面顶部提示）
 */
async function load(manual = false): Promise<void> {
  if (loading.value) return;
  loading.value = true;
  refreshing.value = manual;
  countdown.value = REFRESH_SECONDS;

  // 两个本地探测与主接口并行：它们互不依赖，串行只会让首屏更慢
  const probes = Promise.all([probeBackendApi(), probeStatic()]);

  try {
    const data = await fetchServerStatus();
    status.value = data;
    errorMessage.value = '';
    errorKind.value = 'none';
    pushSample(data);
    const targets = {
      cpu: Number(data.cpu.percent) || 0,
      memory: Number(data.memory.percent) || 0,
      disk: Number(data.disk.percent) || 0,
    };
    if (!ready.value) {
      // 首帧：先把环重置到 0，再在下一帧推到实际值，动画才看得见
      Object.assign(animated, { cpu: 0, memory: 0, disk: 0 });
      window.requestAnimationFrame(() => {
        ready.value = true;
        animateRings(targets);
      });
    } else {
      animateRings(targets);
    }
  } catch (error) {
    const message = error instanceof ApiError ? error.message : '网络异常，请稍后重试';
    errorMessage.value = message;
    const status404 = error instanceof ApiError ? error.status : undefined;
    errorKind.value = status404 === 403 ? 'forbidden' : 'failed';
    if (manual) {
      void MessagePlugin.error(message || '采集失败');
    }
  } finally {
    await probes;
    loading.value = false;
    refreshing.value = false;
  }
}

// ==================== 自动刷新 ====================

let timer: number | undefined;

/** 每秒 tick：隐藏标签页时不倒计时（也不发请求），回到前台立即补一次 */
function startTimer(): void {
  stopTimer();
  timer = window.setInterval(() => {
    if (document.hidden) return;
    countdown.value -= 1;
    if (countdown.value <= 0) {
      void load(false);
    }
  }, 1000);
}

function stopTimer(): void {
  if (timer !== undefined) {
    window.clearInterval(timer);
    timer = undefined;
  }
}

/** 页面从后台回到前台：数据可能已过期，立刻刷新一次 */
function onVisibilityChange(): void {
  if (!document.hidden) {
    void load(false);
  }
}

// ==================== 诊断包 ====================

/** 与后端 `Content-Disposition` 同规则的文件名（blob 分支拿不到响应头，前端自己拼） */
function diagnosticFileName(): string {
  const now = new Date();
  const pad = (n: number) => String(n).padStart(2, '0');
  return `cloudpivot-diagnostic-${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}-${pad(
    now.getHours(),
  )}${pad(now.getMinutes())}${pad(now.getSeconds())}.zip`;
}

/** 下载诊断包（zip 含 info/health/status/metrics/config/migrations/logs，密码已由后端打码） */
async function onDownload(): Promise<void> {
  if (downloading.value) return;
  downloading.value = true;
  try {
    const blob = await downloadDiagnosticBundle();
    if (!blob || blob.size === 0) {
      void MessagePlugin.warning('诊断包为空，请稍后重试');
      return;
    }
    saveBlob(blob, diagnosticFileName());
    void MessagePlugin.success(`诊断包已开始下载（${(blob.size / 1024).toFixed(0)} KB）`);
  } catch (error) {
    const message = error instanceof ApiError ? error.message : '下载失败，请稍后重试';
    void MessagePlugin.error(message);
  } finally {
    downloading.value = false;
  }
}

// ==================== 生命周期 ====================

onMounted(() => {
  loadSamples();
  void load(false);
  startTimer();
  document.addEventListener('visibilitychange', onVisibilityChange);
});

onBeforeUnmount(() => {
  stopTimer();
  window.cancelAnimationFrame(rafId);
  document.removeEventListener('visibilitychange', onVisibilityChange);
});
</script>

<style scoped lang="less">
/**
 * 服务器状态页样式。
 *
 * 约定（与首页一致）：
 *   · 卡片间距 16px、卡片内 padding 20px、圆角 12px、表面色取 --app-card-bg；
 *   · 颜色全部走主题变量（换配色 / 明暗模式无需改这里）；
 *   · 动效克制：环形 800ms、卡片 hover 200ms、骨架流光 1.4s。
 */
@keyframes ss-shimmer {
  from {
    background-position: 100% 0;
  }

  to {
    background-position: 0 0;
  }
}

@keyframes ss-pulse {
  0%,
  100% {
    opacity: 1;
  }

  50% {
    opacity: 0.45;
  }
}

.server-status {
  display: flex;
  flex-direction: column;
  gap: 16px;
  padding-bottom: 8px;
}

/* ==================== 头部 ==================== */
.ss-hero {
  display: flex;
  gap: 16px;
  align-items: center;
  justify-content: space-between;
  padding: 20px 24px;
  border: 1px solid var(--td-component-stroke);
  border-radius: 12px;
  background:
    linear-gradient(120deg, var(--td-brand-color-1) 0%, var(--app-card-bg) 58%, var(--td-brand-color-1) 100%);
  box-shadow: var(--td-shadow-1);

  &__left {
    display: flex;
    gap: 14px;
    align-items: center;
    min-width: 0;
  }

  &__badge {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 44px;
    height: 44px;
    color: var(--td-brand-color-7);
    font-size: 22px;
    background: var(--td-brand-color-2);
    border: 1px solid var(--td-brand-color-4);
    border-radius: 12px;
  }

  &__text {
    min-width: 0;
  }

  &__title {
    margin: 0 0 4px;
    color: var(--td-text-color-primary);
    font-weight: 600;
    font-size: 20px;
    line-height: 1.3;
  }

  &__sub {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: center;
    margin: 0;
    color: var(--td-text-color-secondary);
    font-size: 13px;
  }

  &__host {
    color: var(--td-text-color-primary);
    font-weight: 500;
  }

  &__sep {
    color: var(--td-text-color-disabled);
  }

  &__right {
    display: flex;
    flex-direction: column;
    gap: 6px;
    align-items: flex-end;
    flex-shrink: 0;
  }

  &__time {
    color: var(--td-text-color-secondary);
    font-size: 12px;
  }
}

/* 总体状态胶囊：颜色按语义取主题变量（正常=项目定义的 success 蓝、警告=落日流金、异常=朱砂） */
.ss-pill {
  display: inline-flex;
  gap: 6px;
  align-items: center;
  padding: 4px 12px;
  color: var(--td-text-color-primary);
  font-weight: 500;
  font-size: 13px;
  border-radius: 999px;

  &__dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: currentcolor;
    animation: ss-pulse 2.4s ease-in-out infinite;
  }

  &--ok {
    color: var(--td-success-color-active);
    background: var(--td-brand-color-1);
    border: 1px solid var(--td-brand-color-3);
  }

  &--warning {
    color: var(--td-warning-color-active);
    background: var(--td-brand-color-1);
    border: 1px solid var(--td-warning-color);
  }

  &--error {
    color: var(--td-error-color-active);
    background: var(--td-bg-color-container);
    border: 1px solid var(--td-error-color);
  }

  &--muted {
    color: var(--td-text-color-secondary);
    background: var(--td-bg-color-secondarycontainer);
    border: 1px solid var(--td-component-stroke);
  }
}

.ss-alert {
  margin: 0;
}

/* ==================== 通用卡片 ==================== */
.ss-card {
  padding: 20px;
  background: var(--app-card-bg);
  border: 1px solid var(--td-component-stroke);
  border-radius: 12px;
  box-shadow: var(--td-shadow-1);
  transition:
    transform 200ms ease,
    box-shadow 200ms ease,
    border-color 200ms ease;
}

/* ==================== ① 指标卡 ==================== */
.ss-metrics {
  display: grid;
  // 4 等宽；<1400px 两列；<1000px 单列（卡面设计要求）
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 16px;

  @media (max-width: 1400px) {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  @media (max-width: 1000px) {
    grid-template-columns: minmax(0, 1fr);
  }
}

.ss-metric {
  display: flex;
  flex-direction: column;
  gap: 10px;
  min-width: 0;

  &:hover {
    transform: translateY(-2px);
    border-color: var(--td-brand-color-4);
    box-shadow: var(--td-shadow-2);
  }

  &__head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  &__label {
    color: var(--td-text-color-secondary);
    font-weight: 500;
    font-size: 13px;
    letter-spacing: 0.02em;
  }

  &__icon {
    color: var(--td-text-color-disabled);
    font-size: 18px;
  }

  &__body {
    display: flex;
    justify-content: center;
    padding: 4px 0 2px;
  }

  &__foot {
    margin: 0;
    overflow: hidden;
    color: var(--td-text-color-primary);
    font-size: 14px;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  &__extra {
    margin: 0;
    overflow: hidden;
    color: var(--td-text-color-secondary);
    font-size: 12px;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  &--skeleton {
    gap: 14px;
  }

  /* 窄屏单列时卡片会很高：环缩到 112、内边距收紧，一屏能多看一行 */
  @media (max-width: 1000px) {
    padding: 16px;

    .ss-ring {
      width: 112px;
      height: 112px;
    }

    .ss-ring__num {
      font-size: 26px;
    }

    .ss-ring--decorate .ss-ring__num {
      font-size: 22px;
    }
  }
}

/* ==================== 环形进度 ==================== */
.ss-ring {
  position: relative;
  width: 132px;
  height: 132px;

  &__svg {
    width: 100%;
    height: 100%;
    transform: rotate(-90deg); // 0% 起点移到 12 点方向
  }

  &__track {
    fill: none;
    stroke: var(--td-component-stroke);
    stroke-width: 9;
  }

  &__value {
    fill: none;
    stroke: var(--td-brand-color-7);
    stroke-width: 9;
    stroke-linecap: round;
    // 环形从 0 长到实际值：改 dashoffset，浏览器补间 800ms
    transition: stroke-dashoffset 800ms cubic-bezier(0.22, 0.61, 0.36, 1);
  }

  &__value--warn {
    stroke: var(--td-warning-color);
  }

  &__value--danger {
    stroke: var(--td-error-color);
  }

  &__center {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 2px;
    pointer-events: none;
  }

  &__num {
    color: var(--td-text-color-primary);
    font-weight: 600;
    font-size: 32px;
    font-variant-numeric: tabular-nums;
    line-height: 1.05;
    transition: color 200ms ease;
  }

  &__unit {
    color: var(--td-text-color-secondary);
    font-size: 12px;
  }

  /* 运行时长没有百分比：轨道做虚线装饰环，只放时长本身 */
  &--decorate {
    .ss-ring__track {
      stroke: var(--td-brand-color-3);
      stroke-width: 3;
      stroke-dasharray: 4 9;
    }

    .ss-ring__num {
      font-size: 28px;
    }
  }
}

/* ==================== ② 趋势图 ==================== */
.ss-charts {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;

  @media (max-width: 1000px) {
    grid-template-columns: minmax(0, 1fr);
  }
}

.ss-chart {
  display: flex;
  flex-direction: column;
  gap: 8px;
  min-width: 0;

  &__head {
    display: flex;
    gap: 12px;
    align-items: flex-start;
    justify-content: space-between;
  }

  &__title {
    margin: 0 0 2px;
    color: var(--td-text-color-primary);
    font-weight: 600;
    font-size: 15px;
  }

  &__sub {
    margin: 0;
    color: var(--td-text-color-secondary);
    font-size: 12px;
  }

  &__badge {
    flex-shrink: 0;
    padding: 2px 8px;
    color: var(--td-text-color-secondary);
    font-size: 12px;
    background: var(--td-bg-color-secondarycontainer);
    border: 1px solid var(--td-component-stroke);
    border-radius: 999px;
  }

  &__empty {
    display: flex;
    flex-direction: column;
    gap: 8px;
    align-items: center;
    justify-content: center;
    height: 228px;
    color: var(--td-text-color-secondary);
    font-size: 13px;
    text-align: center;
    background:
      repeating-linear-gradient(
        0deg,
        transparent 0,
        transparent 45px,
        var(--td-component-stroke) 45px,
        var(--td-component-stroke) 46px
      );
    border-radius: 8px;

    :deep(.t-icon) {
      color: var(--td-text-color-disabled);
      font-size: 28px;
    }

    p {
      max-width: 340px;
      margin: 0;
      line-height: 1.6;
    }
  }
}

/* ==================== ③ 服务健康 ==================== */
.ss-section {
  &__head {
    display: flex;
    gap: 12px;
    align-items: flex-start;
    justify-content: space-between;
    margin-bottom: 12px;
  }

  &__title {
    margin: 0 0 2px;
    color: var(--td-text-color-primary);
    font-weight: 600;
    font-size: 15px;
  }

  &__sub {
    margin: 0;
    color: var(--td-text-color-secondary);
    font-size: 12px;
  }

  &__count {
    flex-shrink: 0;
    color: var(--td-text-color-secondary);
    font-size: 12px;
  }
}

.ss-health {
  &__table {
    display: flex;
    flex-direction: column;
  }

  &__row {
    display: grid;
    grid-template-columns: minmax(140px, 1.1fr) 92px 100px 96px minmax(160px, 2fr);
    gap: 12px;
    align-items: center;
    padding: 11px 8px;
    border-bottom: 1px solid var(--td-component-stroke);
    transition: background-color 160ms ease;

    &:last-child {
      border-bottom: none;
    }

    /* 表头不参与 hover 高亮：单独压掉，避免 `:not(&--head)` 这种 LESS 嵌套陷阱 */
    &:hover {
      background: var(--td-bg-color-container-hover);
      border-radius: 8px;
    }

    &--head {
      padding: 6px 8px;
      color: var(--td-text-color-secondary);
      font-weight: 500;
      font-size: 12px;
      letter-spacing: 0.02em;

      &:hover {
        background: transparent;
      }
    }

    // 异常 / 警告行左侧加一条语义色，扫视时不用逐行读文字
    &--error {
      .ss-health__name {
        color: var(--td-error-color-active);
      }

      .ss-health__dot {
        background: var(--td-error-color);
      }
    }

    &--warning {
      .ss-health__name {
        color: var(--td-warning-color-active);
      }

      .ss-health__dot {
        background: var(--td-warning-color);
      }
    }

    &--ok .ss-health__dot {
      background: var(--td-success-color);
    }

    &--muted {
      .ss-health__name {
        color: var(--td-text-color-secondary);
      }

      .ss-health__dot {
        background: var(--td-text-color-disabled);
      }
    }

    @media (max-width: 1000px) {
      grid-template-columns: minmax(120px, 1fr) 84px 84px;
      row-gap: 2px;

      /* 窄屏只保留 服务 / 状态 / 响应时间，说明换到下一行整行显示 */
      > :nth-child(4) {
        display: none;
      }

      .ss-health__detail {
        grid-column: 1 / -1;
        padding-left: 14px;
        color: var(--td-text-color-secondary);
        font-size: 12px;
      }
    }
  }

  &__name {
    display: inline-flex;
    gap: 8px;
    align-items: center;
    min-width: 0;
    color: var(--td-text-color-primary);
    font-size: 14px;
  }

  &__dot {
    flex-shrink: 0;
    width: 8px;
    height: 8px;
    background: var(--td-text-color-disabled);
    border-radius: 50%;
  }

  &__mono {
    color: var(--td-text-color-secondary);
    font-size: 13px;
    font-variant-numeric: tabular-nums;
  }

  &__detail {
    overflow: hidden;
    color: var(--td-text-color-secondary);
    font-size: 13px;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
}

/* 状态标签：底色一律用品牌色最浅阶 + 语义色文字，保证深色模式也能读 */
.ss-tag {
  display: inline-flex;
  align-items: center;
  padding: 1px 8px;
  font-weight: 500;
  font-size: 12px;
  border-radius: 4px;

  &--ok {
    color: var(--td-success-color-active);
    background: var(--td-brand-color-1);
  }

  &--warning {
    color: var(--td-warning-color-active);
    background: var(--td-brand-color-1);
  }

  &--error {
    color: var(--td-error-color-active);
    background: var(--td-bg-color-secondarycontainer);
  }

  &--muted {
    color: var(--td-text-color-secondary);
    background: var(--td-bg-color-secondarycontainer);
  }
}

/* ==================== ④ 环境信息 ==================== */
.ss-env {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;

  @media (max-width: 1000px) {
    grid-template-columns: minmax(0, 1fr);
  }

  &__card {
    min-width: 0;

    &:hover {
      transform: translateY(-2px);
      border-color: var(--td-brand-color-4);
      box-shadow: var(--td-shadow-2);
    }
  }

  &__head {
    display: flex;
    gap: 8px;
    align-items: center;
    margin-bottom: 12px;
  }

  &__icon {
    color: var(--td-brand-color-7);
    font-size: 16px;
  }

  &__title {
    margin: 0;
    color: var(--td-text-color-primary);
    font-weight: 600;
    font-size: 14px;
  }

  &__list {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin: 0;
  }

  &__item {
    display: flex;
    gap: 12px;
    align-items: baseline;
    justify-content: space-between;
    min-width: 0;
    padding-bottom: 8px;
    border-bottom: 1px dashed var(--td-component-stroke);

    &:last-child {
      padding-bottom: 0;
      border-bottom: none;
    }

    dt {
      flex-shrink: 0;
      color: var(--td-text-color-secondary);
      font-size: 13px;
    }

    dd {
      margin: 0;
      overflow: hidden;
      color: var(--td-text-color-primary);
      font-size: 13px;
      white-space: nowrap;
      text-overflow: ellipsis;
    }
  }

  &__value--mono {
    font-family: 'SFMono-Regular', 'JetBrains Mono', Consolas, 'Liberation Mono', Menlo, monospace;
    font-size: 12px;
  }
}

/* ==================== ⑤ 底部操作栏 ==================== */
.ss-actions {
  position: sticky;
  bottom: 0;
  z-index: 5;
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  align-items: center;
  justify-content: space-between;
  margin-top: 4px;
  padding: 12px 16px;
  background: color-mix(in srgb, var(--app-card-bg) 88%, transparent);
  border: 1px solid var(--td-component-stroke);
  border-radius: 12px;
  box-shadow: var(--td-shadow-2);
  backdrop-filter: blur(8px);

  &__buttons {
    display: flex;
    gap: 10px;
    align-items: center;
  }

  &__hint {
    margin: 0;
    color: var(--td-text-color-secondary);
    font-size: 12px;
    font-variant-numeric: tabular-nums;
  }

  &__sep {
    margin: 0 6px;
    color: var(--td-text-color-disabled);
  }
}

/* ==================== 骨架 / 空态 ==================== */
.ss-skeleton {
  background:
    linear-gradient(
      90deg,
      var(--td-bg-color-secondarycontainer) 25%,
      var(--td-bg-color-container-hover) 37%,
      var(--td-bg-color-secondarycontainer) 63%
    );
  background-size: 400% 100%;
  border-radius: 8px;
  animation: ss-shimmer 1.4s ease-in-out infinite;

  &--label {
    width: 42%;
    height: 14px;
  }

  &--ring {
    align-self: center;
    width: 132px;
    height: 132px;
    margin: 4px 0;
    border-radius: 50%;
  }

  &--line {
    width: 68%;
    height: 12px;
  }
}

.ss-empty {
  display: flex;
  flex-direction: column;
  gap: 10px;
  align-items: center;
  justify-content: center;
  padding: 56px 24px;
  text-align: center;

  &__icon {
    color: var(--td-text-color-disabled);
    font-size: 40px;
  }

  &__title {
    margin: 0;
    color: var(--td-text-color-primary);
    font-weight: 600;
    font-size: 16px;
  }

  &__desc {
    max-width: 460px;
    margin: 0;
    color: var(--td-text-color-secondary);
    font-size: 13px;
    line-height: 1.7;
  }
}
</style>

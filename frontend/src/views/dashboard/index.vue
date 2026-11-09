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
          右栏：固定 360px，整块只放「公告与消息」卡 —— 所有人可见（公告 / 消息本身就是全员数据），
          不再按角色收口。上区 5 条公告、下区 10 条消息；点条目在本页抽屉里读详情（不跳走，
          回到首页时状态不丢）。未读红点取接收行自己的 read_flag，点开即标已读并同步 notice store，
          这样顶栏铃铛的角标不会和卡里的红点对不上。
        -->
        <t-col class="dashboard__aside">
          <t-card title="公告与消息" :bordered="false" class="dashboard__card dashboard__feed">
            <template #actions>
              <div class="dashboard__feed-actions">
                <t-link theme="primary" hover="color" @click="goTo(ANNOUNCEMENT_PATH)">
                  公告全部
                </t-link>
                <span class="dashboard__feed-actions-sep">/</span>
                <t-link theme="primary" hover="color" @click="goTo(MESSAGE_PATH)">消息全部</t-link>
              </div>
            </template>

            <!-- 首屏骨架：首次加载不白屏；二次刷新保留旧内容（不闪骨架） -->
            <div v-if="feedLoading && !feedLoaded" class="dashboard__feed-skeleton">
              <div v-for="row in 12" :key="row" class="dashboard__feed-skeleton-row" />
            </div>

            <template v-else>
              <!-- 上半区：公告 5 条 -->
              <section class="dashboard__feed-section">
                <div class="dashboard__feed-head">
                  <t-icon class="dashboard__feed-head-icon dashboard__feed-head-icon--ann" name="notification" />
                  <span class="dashboard__feed-head-title">公告</span>
                  <span class="dashboard__feed-head-count">共 {{ announcementTotal }} 条</span>
                </div>
                <ul v-if="announcements.length" class="dashboard__feed-list">
                  <li v-for="item in announcements" :key="item.id">
                    <button type="button" class="dashboard__feed-item" @click="openAnnouncement(item)">
                      <span class="dashboard__feed-item-title" :title="item.title">{{ item.title }}</span>
                      <span class="dashboard__feed-item-time">{{ formatFeedTime(item.publish_at) }}</span>
                    </button>
                  </li>
                </ul>
                <div v-else class="dashboard__feed-empty">
                  <t-icon name="notification" />
                  <span>暂无公告</span>
                </div>
              </section>

              <!-- 下半区：消息 10 条（未读带红点） -->
              <section class="dashboard__feed-section dashboard__feed-section--notice">
                <div class="dashboard__feed-head">
                  <t-icon class="dashboard__feed-head-icon dashboard__feed-head-icon--msg" name="chat" />
                  <span class="dashboard__feed-head-title">消息</span>
                  <span class="dashboard__feed-head-count">共 {{ noticeTotal }} 条</span>
                </div>
                <ul v-if="notices.length" class="dashboard__feed-list">
                  <li v-for="item in notices" :key="item.receiver_id">
                    <button
                      type="button"
                      class="dashboard__feed-item"
                      :class="{ 'dashboard__feed-item--unread': !item.read_flag }"
                      @click="openNotice(item)"
                    >
                      <span class="dashboard__feed-item-title" :title="item.title">
                        <span v-if="!item.read_flag" class="dashboard__feed-dot" aria-label="未读" />
                        {{ item.title }}
                      </span>
                      <span class="dashboard__feed-item-time">{{ formatFeedTime(item.created_at) }}</span>
                    </button>
                  </li>
                </ul>
                <div v-else class="dashboard__feed-empty">
                  <t-icon name="chat" />
                  <span>暂无消息</span>
                </div>
              </section>

              <!-- 底部统计条：条数按「本卡展示的」算（公告 ≤5 / 消息 ≤10），与顶部「共 N 条」区分 -->
              <div class="dashboard__feed-foot">
                <span>公告 {{ announcements.length }} 条</span>
                <span class="dashboard__feed-foot-sep">·</span>
                <span>消息 {{ notices.length }} 条</span>
                <span v-if="feedUnread > 0" class="dashboard__feed-foot-unread">（未读 {{ feedUnread }}）</span>
              </div>

              <div v-if="feedError" class="dashboard__feed-error">
                <t-icon name="error-circle" />
                <span>{{ feedError }}</span>
                <t-link theme="primary" hover="color" @click="loadFeed()">重试</t-link>
              </div>
            </template>
          </t-card>
        </t-col>
      </t-row>
    </t-loading>

    <!--
      详情抽屉：首页只做「扫一眼」，点开读全文不跳页 —— 读完关掉还在首页，
      不会丢掉「刚登录想看什么」的上下文。公告与消息各一个，互不干扰。
    -->
    <t-drawer
      v-model:visible="announcementDrawerVisible"
      header="公告详情"
      size="560px"
      :footer="false"
      destroy-on-close
    >
      <t-loading :loading="announcementDetailLoading" size="small">
        <template v-if="announcementDetail">
          <h3 class="dashboard__detail-title">
            <t-icon v-if="announcementDetail.is_top" class="dashboard__detail-pin" name="arrow-up" />
            {{ announcementDetail.title }}
          </h3>
          <div class="dashboard__detail-meta">
            <t-tag size="small" variant="light" :theme="levelTheme(announcementDetail.level)">
              {{ levelLabel(announcementDetail.level) }}
            </t-tag>
            <span>{{ announcementDetail.ann_no }}</span>
            <span>发布人：{{ announcementDetail.publisher_name || '系统' }}</span>
            <span>{{ formatDateTime(announcementDetail.publish_at) }}</span>
          </div>
          <MarkdownViewer
            :html="announcementDetail.content_html"
            :show-toc="false"
            empty-text="公告正文为空"
          />
          <div v-if="announcementDetail.attachments.length" class="dashboard__detail-files">
            <div class="dashboard__detail-files-title">
              附件（{{ announcementDetail.attachments.length }}）
            </div>
            <button
              v-for="file in announcementDetail.attachments"
              :key="file.id"
              type="button"
              class="dashboard__detail-file"
              @click="downloadAttachment(file)"
            >
              <t-icon name="file" />
              <span class="dashboard__detail-file-name" :title="file.file_name">{{ file.file_name }}</span>
              <span class="dashboard__detail-file-size">{{ formatFileSize(file.file_size) }}</span>
            </button>
          </div>
        </template>
        <div v-else-if="!announcementDetailLoading" class="dashboard__feed-empty">
          <t-icon name="notification" />
          <span>公告内容加载失败</span>
        </div>
      </t-loading>
    </t-drawer>

    <t-drawer v-model:visible="noticeDrawerVisible" header="消息详情" size="480px" :footer="false">
      <template v-if="activeNotice">
        <h3 class="dashboard__detail-title">{{ activeNotice.title }}</h3>
        <div class="dashboard__detail-meta">
          <t-tag size="small" variant="light" :theme="levelTheme(activeNotice.level)">
            {{ levelLabel(activeNotice.level) }}
          </t-tag>
          <span>{{ noticeTypeLabel(activeNotice.notice_type) }}</span>
          <span>{{ formatDateTime(activeNotice.created_at) }}</span>
          <span v-if="activeNotice.read_flag" class="dashboard__detail-read">已读</span>
        </div>
        <div class="dashboard__detail-body" v-html="renderNoticeContent(activeNotice)" />
        <div v-if="activeNotice.link_url" class="dashboard__detail-link">
          <t-link theme="primary" @click="openNoticeLink(activeNotice.link_url)">查看详情 →</t-link>
        </div>
      </template>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { useRouter } from 'vue-router';
import dayjs from 'dayjs';
import type { EChartsCoreOption } from 'echarts';
import { MessagePlugin } from 'tdesign-vue-next';
import EChart from '@/components/EChart.vue';
import MarkdownViewer from '@/components/MarkdownViewer.vue';
import {
  downloadAnnouncementAttachment,
  getAnnouncementDetail,
  markAnnouncementRead,
  pageVisibleAnnouncements,
  type AnnouncementAttachment,
  type AnnouncementCard,
  type AnnouncementDetail,
} from '@/api/business/announcement-api';
import {
  fetchUnreadCount,
  markNoticeRead,
  pageMyNotices,
  type MyNoticeRes,
} from '@/api/business/notice';
import { fetchOverview, type OverviewRes } from '@/api/support/dashboard-api';
import { ApiError } from '@/api/request';
import { useNoticeStore } from '@/stores/notice';
import { useUserStore } from '@/stores/user';
import { formatDateTime, formatFileSize } from '@/utils/format';
import { saveBlob } from '@/utils/download';

/** 工作台概览数据，加载中或未加载时为 null */
const overview = ref<OverviewRes | null>(null);
/** 概览加载中标记 */
const isLoading = ref(false);
/** 概览加载失败提示（不阻断页面渲染，图表降级为空图） */
const overviewError = ref('');

// ---------- 公告与消息卡（右栏） ----------

/** 公告中心路由（与 `router/index.ts` 的 `AnnouncementCenter` 逐字一致） */
const ANNOUNCEMENT_PATH = '/announcement';
/** 消息中心路由（`BusinessNoticeMessage`，菜单隐藏，从铃铛进） */
const MESSAGE_PATH = '/business/notice/message';
/** 首页公告区最多几条 */
const ANNOUNCEMENT_LIMIT = 5;
/** 首页消息区最多几条 */
const NOTICE_LIMIT = 10;

/** 公告列表（最多 5 条） */
const announcements = ref<AnnouncementCard[]>([]);
/** 公告总数（分页接口返回的 total，不是本页条数） */
const announcementTotal = ref(0);
/** 消息列表（最多 10 条） */
const notices = ref<MyNoticeRes[]>([]);
/** 消息总数 */
const noticeTotal = ref(0);
/** 卡片数据加载中标记 */
const feedLoading = ref(false);
/** 是否已成功拉过一次（决定要不要显示骨架屏；刷新时不再闪骨架） */
const feedLoaded = ref(false);
/** 卡片加载失败提示 */
const feedError = ref('');

/** 公告详情抽屉显隐 */
const announcementDrawerVisible = ref(false);
/** 公告详情加载中 */
const announcementDetailLoading = ref(false);
/** 公告详情 */
const announcementDetail = ref<AnnouncementDetail | null>(null);
/** 消息详情抽屉显隐 */
const noticeDrawerVisible = ref(false);
/** 当前打开的消息（列表行本身就带正文，无需再请求详情） */
const activeNotice = ref<MyNoticeRes | null>(null);

const noticeStore = useNoticeStore();
/**
 * 底部统计条的「未读」= 本卡展示的这 10 条里还没读的条数。
 *
 * 不直接用 store 的全量未读（可能有几十条没出现在卡里）：写成「消息 10 条（未读 17）」
 * 会让用户以为这 10 条里有 17 条没读。全量未读由顶栏铃铛负责，两处口径各司其职。
 */
const feedUnread = computed(() => notices.value.filter((item) => !item.read_flag).length);

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

// ---------- 公告与消息卡：加载与交互 ----------

/** 列表时间只到分钟（`2026-11-09 10:23`）：秒级精度在卡片里只是噪声 */
function formatFeedTime(value?: string | null): string {
  if (!value) return '-';
  const parsed = dayjs(value);
  return parsed.isValid() ? parsed.format('YYYY-MM-DD HH:mm') : '-';
}

/** 级别 → 中文（公告与消息共用一张映射表，未登记的级别原样显示） */
function levelLabel(level: string): string {
  const map: Record<string, string> = {
    NORMAL: '普通',
    IMPORTANT: '重要',
    URGENT: '紧急',
    INFO: '通知',
    SUCCESS: '成功',
    WARN: '警告',
    ERROR: '错误',
  };
  return map[(level || '').toUpperCase()] || level || '普通';
}

/** 级别 → 语义色（状态色语义固定，不跟配色方案走） */
function levelTheme(level: string): 'primary' | 'success' | 'warning' | 'danger' {
  const key = (level || '').toUpperCase();
  if (key === 'URGENT' || key === 'ERROR') return 'danger';
  if (key === 'IMPORTANT' || key === 'WARN') return 'warning';
  if (key === 'SUCCESS') return 'success';
  return 'primary';
}

/** 消息类型 → 中文 */
function noticeTypeLabel(type: string): string {
  const map: Record<string, string> = {
    SYSTEM: '系统消息',
    TODO: '待办',
    ALERT: '告警',
    WORKFLOW: '审批',
    CIRCULAR: '公告',
  };
  return map[(type || '').toUpperCase()] || type || '消息';
}

/** 消息正文：TEXT 先转义再折行，HTML 原样（后端渲染时已转义变量），与消息中心同一口径 */
function renderNoticeContent(item: MyNoticeRes): string {
  const raw = item.content || '';
  if ((item.content_format || 'TEXT').toUpperCase() === 'HTML') {
    return raw;
  }
  return raw
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/\n/g, '<br />');
}

/**
 * 拉卡片数据：公告 5 条、消息 10 条、未读数三路并发。
 *
 * 用 `allSettled` 而不是 `all`：任意一路失败只让对应半区显示空态，
 * 另一区照常可用 —— 首页不该因为「未读数接口抽风」整块变空。
 */
async function loadFeed(): Promise<void> {
  feedLoading.value = true;
  feedError.value = '';
  const [annRes, noticeRes, unreadRes] = await Promise.allSettled([
    pageVisibleAnnouncements({ page: 1, page_size: ANNOUNCEMENT_LIMIT }),
    pageMyNotices({ page: 1, page_size: NOTICE_LIMIT }),
    fetchUnreadCount(),
  ]);

  if (annRes.status === 'fulfilled') {
    // 接口已按发布时间倒序，这里再截一刀兜底「最多 5 条」的硬约束
    announcements.value = annRes.value.list.slice(0, ANNOUNCEMENT_LIMIT);
    announcementTotal.value = annRes.value.total;
  } else {
    announcements.value = [];
    announcementTotal.value = 0;
  }

  if (noticeRes.status === 'fulfilled') {
    notices.value = noticeRes.value.list.slice(0, NOTICE_LIMIT);
    noticeTotal.value = noticeRes.value.total;
  } else {
    notices.value = [];
    noticeTotal.value = 0;
  }

  if (unreadRes.status === 'fulfilled') {
    noticeStore.setUnreadCount(unreadRes.value.unread_count);
  }

  // 两区都失败才值得打扰用户；单区失败由空态文案说明，不弹错误条
  const failed = [annRes, noticeRes].filter((res) => res.status === 'rejected');
  if (failed.length === 2) {
    const reason = (failed[0] as PromiseRejectedResult).reason;
    feedError.value = reason instanceof ApiError ? reason.message : '公告与消息加载失败';
  }

  feedLoaded.value = true;
  feedLoading.value = false;
}

/** 打开公告详情（点开即读，标已读幂等） */
async function openAnnouncement(item: AnnouncementCard): Promise<void> {
  announcementDrawerVisible.value = true;
  announcementDetail.value = null;
  announcementDetailLoading.value = true;
  try {
    const detail = await getAnnouncementDetail(item.id);
    announcementDetail.value = detail;
    if (!detail.read) {
      try {
        await markAnnouncementRead(detail.id);
        detail.read = true;
        item.read = true;
      } catch (error) {
        // 标已读失败不影响阅读，但要让用户知道红点没消
        void MessagePlugin.warning(error instanceof ApiError ? error.message : '标记已读失败');
      }
    }
  } catch (error) {
    void MessagePlugin.error(error instanceof ApiError ? error.message : '公告详情加载失败');
  } finally {
    announcementDetailLoading.value = false;
  }
}

/** 打开消息详情：列表行本身带正文，直接展示；未读则顺手标已读并同步角标 */
async function openNotice(item: MyNoticeRes): Promise<void> {
  activeNotice.value = item;
  noticeDrawerVisible.value = true;
  if (item.read_flag) return;
  try {
    await markNoticeRead(item.receiver_id);
    item.read_flag = true;
    item.read_at = new Date().toISOString();
    // 顶栏角标与本卡共用 store，改一处即可
    noticeStore.setUnreadCount(noticeStore.unreadCount - 1);
  } catch (error) {
    void MessagePlugin.error(error instanceof ApiError ? error.message : '标记已读失败');
  }
}

/** 消息里的跳转地址：新窗口打开，不打断首页上下文 */
function openNoticeLink(url: string): void {
  window.open(url, '_blank', 'noopener');
}

/** 下载公告附件（代理接口要鉴权头，先取 blob 再保存） */
async function downloadAttachment(file: AnnouncementAttachment): Promise<void> {
  try {
    const blob = await downloadAnnouncementAttachment(file.download_url);
    saveBlob(blob, file.file_name);
  } catch (error) {
    void MessagePlugin.error(error instanceof ApiError ? error.message : '附件下载失败');
  }
}

onMounted(() => {
  void loadOverview();
  void loadFeed();
});
</script>

<style scoped lang="less">
// 骨架屏流光：@keyframes 必须放顶层，嵌在选择器里 LESS 不会提升
@keyframes dashboard-feed-shimmer {
  from {
    background-position: 100% 0;
  }

  to {
    background-position: 0 0;
  }
}

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
    // 白字在深色品牌段上，加一点深色微阴影提升边缘清晰度
    text-shadow: 0 1px 3px rgb(0 0 0 / 26%);
  }

  &__subtitle {
    margin: 0;
    font-size: 13px;
    // 渐变终点是浅色，副标题用同色文字（略透明以区分层级，主色端仍 ≥4.5）
    color: var(--app-on-gradient, #ffffff);
    opacity: 0.94;
    // 白字在深色段已足够清晰，给一点深色微阴影防止后段浅色区发虚
    text-shadow: 0 1px 2px rgb(0 0 0 / 22%);
  }

  &__banner-icon {
    font-size: 72px;
    // 图标在横幅右侧（浅色段），必须用深色油墨才可见
    color: var(--app-on-gradient-soft, #1a2230);
    opacity: 0.16;
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
    // 右栏按自身内容高度排列：左栏（指标 + 快捷入口 + 两张图）通常更高，
    // 若跟着 stretch 拉伸，卡片底部会拖出一大片空白。
    align-self: flex-start;
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

  // ---------- 公告与消息卡（右栏，全员可见） ----------
  // 高度由内容撑开：5 条公告 + 10 条消息 ≈ 560px，正好占满右栏首屏。
  // 卡片自身不设固定高度 —— 数据少时短、多时到上限截断，不会留死白。
  &__feed {
    height: auto;

    // t-card 默认内边距 24px；条目行自身已有 5px 留白，收紧到 6px 12px 12px
    :deep(.t-card__body) {
      padding: 6px 12px 12px;
    }

    // 头部右上角两个入口：公告全部 / 消息全部
    &-actions {
      display: flex;
      align-items: center;
      gap: 8px;
      font-size: 13px;
    }

    &-actions-sep {
      color: var(--td-text-color-placeholder);
    }

    // 首屏骨架：12 行灰条，避免「先白屏再跳内容」
    &-skeleton {
      display: flex;
      flex-direction: column;
      gap: 10px;
      padding: 8px 4px;
    }

    &-skeleton-row {
      height: 13px;
      background: linear-gradient(
        90deg,
        var(--td-bg-color-container-hover) 25%,
        var(--td-bg-color-secondarycontainer) 37%,
        var(--td-bg-color-container-hover) 63%
      );
      background-size: 400% 100%;
      border-radius: var(--td-radius-default);
      animation: dashboard-feed-shimmer 1.4s ease infinite;

      &:nth-child(2n) {
        width: 72%;
      }

      &:nth-child(3n) {
        width: 86%;
      }
    }

    // 上下两区：公告区与消息区之间用细线分隔
    // 行高按「15 行 + 2 个区头 + 统计条」压到 ~600px（5+10 条正好一屏，不多留白）
    &-section {
      padding: 6px 0 2px;

      & + & {
        border-top: 1px solid var(--td-component-stroke);
      }
    }

    &-head {
      display: flex;
      align-items: center;
      gap: 6px;
      padding: 0 4px 2px;
    }

    &-head-icon {
      font-size: 15px;

      &--ann {
        color: var(--td-brand-color);
      }

      &--msg {
        color: var(--td-warning-color);
      }
    }

    &-head-title {
      font-size: 13px;
      font-weight: 600;
      color: var(--td-text-color-primary);
    }

    &-head-count {
      margin-left: auto;
      font-size: 12px;
      color: var(--td-text-color-placeholder);
      font-variant-numeric: tabular-nums;
    }

    &-list {
      margin: 0;
      padding: 0;
      list-style: none;
    }

    // 整行可点：button 重置成文本行，键盘 Tab 也能到达（focus-visible 保留焦点环）
    &-item {
      display: flex;
      gap: 10px;
      align-items: baseline;
      width: 100%;
      padding: 3px 6px;
      font: inherit;
      color: inherit;
      text-align: left;
      cursor: pointer;
      background: none;
      border: 0;
      border-radius: var(--td-radius-default);
      transition: background-color 0.15s ease;

      &:hover {
        background-color: var(--td-bg-color-container-hover);
      }

      &:focus-visible {
        outline: 2px solid var(--td-brand-color);
        outline-offset: -2px;
      }

      // 未读：标题加深加粗 + 前置红点，扫一眼就能区分
      &--unread {
        .dashboard__feed-item-title {
          font-weight: 600;
          color: var(--td-text-color-primary);
        }
      }
    }

    &-item-title {
      display: flex;
      flex: 1 1 auto;
      gap: 6px;
      align-items: center;
      min-width: 0;
      overflow: hidden;
      font-size: 13px;
      color: var(--td-text-color-secondary);
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    &-item-time {
      flex: none;
      font-size: 12px;
      color: var(--td-text-color-placeholder);
      font-variant-numeric: tabular-nums;
    }

    &-dot {
      flex: none;
      width: 6px;
      height: 6px;
      background-color: var(--td-error-color);
      border-radius: 50%;
    }

    &-empty {
      display: flex;
      flex-direction: column;
      gap: 6px;
      align-items: center;
      padding: 18px 0;
      font-size: 12px;
      color: var(--td-text-color-placeholder);
    }

    &-foot {
      display: flex;
      gap: 6px;
      align-items: center;
      margin-top: 8px;
      padding: 8px 4px 0;
      font-size: 12px;
      color: var(--td-text-color-placeholder);
      border-top: 1px solid var(--td-component-stroke);
      font-variant-numeric: tabular-nums;
    }

    &-foot-sep {
      color: var(--td-text-color-placeholder);
    }

    &-foot-unread {
      color: var(--td-error-color);
    }

    &-error {
      display: flex;
      gap: 6px;
      align-items: center;
      margin-top: 8px;
      padding: 6px 8px;
      font-size: 12px;
      color: var(--td-error-color);
      background-color: var(--td-error-color-1);
      border-radius: var(--td-radius-default);
    }
  }

  // ---------- 公告 / 消息详情抽屉 ----------
  &__detail-title {
    display: flex;
    gap: 6px;
    align-items: center;
    margin: 0 0 10px;
    font-size: 17px;
    font-weight: 600;
    line-height: 1.4;
    color: var(--td-text-color-primary);
  }

  &__detail-pin {
    flex: none;
    color: var(--td-brand-color);
  }

  &__detail-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    align-items: center;
    padding-bottom: 12px;
    margin-bottom: 14px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
    border-bottom: 1px solid var(--td-component-stroke);
  }

  &__detail-read {
    color: var(--td-success-color);
  }

  &__detail-body {
    font-size: 14px;
    line-height: 1.75;
    color: var(--td-text-color-primary);
    word-break: break-word;

    :deep(img) {
      max-width: 100%;
    }
  }

  &__detail-link {
    margin-top: 16px;
    font-size: 13px;
  }

  &__detail-files {
    margin-top: 20px;
    padding-top: 12px;
    border-top: 1px solid var(--td-component-stroke);
  }

  &__detail-files-title {
    margin-bottom: 8px;
    font-size: 13px;
    color: var(--td-text-color-secondary);
  }

  &__detail-file {
    display: flex;
    gap: 8px;
    align-items: center;
    width: 100%;
    padding: 8px 10px;
    margin-bottom: 6px;
    font: inherit;
    color: var(--td-text-color-primary);
    text-align: left;
    cursor: pointer;
    background-color: var(--td-bg-color-container-hover);
    border: 0;
    border-radius: var(--td-radius-default);
    transition: background-color 0.15s ease;

    &:hover {
      background-color: var(--td-bg-color-secondarycontainer);
    }
  }

  &__detail-file-name {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    font-size: 13px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  &__detail-file-size {
    flex: none;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
    font-variant-numeric: tabular-nums;
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

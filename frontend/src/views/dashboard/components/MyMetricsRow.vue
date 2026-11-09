<!--
  工作台顶部「我的指标」四张统计卡（HOME-6）。

  数据来自独立接口 `GET /api/business/dashboard/my-metrics`（登录即可，只回自己的数）：
  待办审批 / 本周已办 / 今日任务 / 逾期。

  为什么单独成接口而不是复用系统信息 Tab 的 overview：
  overview 的 metrics 是**系统级**口径（角色总数、菜单总数、近 7 天操作日志…），
  只给超管看；把这四张「我自己的」卡塞进去会让超管视图出现
  「角色总数 12」和「我的待办 3」并列的语义混乱。两边各自演进、互不影响。

  口径复用后端，前端只做展示与跳转：
  · 「待办审批」的键 `my_todo_count` 与审批中心「待我审批」Tab 同一个 `total`（同一函数）；
  · 「本周已办」= 同接口 `completed_at >= 本周一 00:00 (CST)`；
  · 「今日任务」= 任务中心「我的任务」的 今天 + 逾期，附加数是今天已完成；
  · 「逾期」> 0 时数字标红 + 卡片左侧 2px 红边（与列表里逾期截止日同一套语义色）。
-->
<template>
  <section class="mm" data-testid="my-metrics">
    <!-- 首屏骨架：四张同尺寸占位，避免「先空一条再跳出来」 -->
    <div v-if="loading && !loaded" class="mm__grid">
      <div v-for="row in 4" :key="row" class="mm__skeleton" />
    </div>

    <template v-else>
      <div class="mm__grid">
        <button
          v-for="card in cards"
          :key="card.key"
          type="button"
          class="mm__card"
          :class="{ 'is-danger': isDanger(card), 'is-static': !routeOf(card.key) }"
          :style="{ '--mm-accent': accentVar(card) }"
          :data-metric="card.key"
          :disabled="!routeOf(card.key)"
          :title="card.tip"
          @click="go(card.key)"
        >
          <span class="mm__icon">
            <t-icon :name="card.icon" />
          </span>
          <span class="mm__body">
            <span class="mm__value">{{ card.value }}<span v-if="card.unit" class="mm__unit">{{ card.unit }}</span></span>
            <span class="mm__label">{{ card.label }}</span>
            <span class="mm__extra">{{ extraText(card) }}</span>
          </span>
        </button>
      </div>

      <!-- 接口异常：不编造数字，也不挡住下面的卡片，只给一条可重试的窄提示 -->
      <div v-if="errorText" class="mm__error">
        <t-icon name="error-circle" />
        <span>{{ errorText }}</span>
        <t-link theme="primary" hover="color" @click="load">重试</t-link>
      </div>
    </template>
  </section>
</template>

<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { useRouter } from 'vue-router';
import { fetchMyMetrics, type MetricCard } from '@/api/support/dashboard-api';
import { ApiError } from '@/api/request';
import { useUserStore } from '@/stores/user';

const router = useRouter();
const userStore = useUserStore();

/** 四张卡（接口固定返回 4 条；异常时为空数组，由错误条说明原因） */
const cards = ref<MetricCard[]>([]);
/** 加载中（只在首屏显示骨架，刷新时保留旧数字不闪） */
const loading = ref(false);
/** 是否已成功拉过一次 */
const loaded = ref(false);
/** 加载失败提示（空串 = 无错） */
const errorText = ref('');

/**
 * 主题键 → CSS 变量。
 *
 * 只用 TDesign 的主题变量（雾蓝流金在 `--td-brand-color` 上，换配色方案时这里不用改）；
 * 未登记的键回落到主色，绝不硬编码色值。
 */
const THEME_VARS: Record<string, string> = {
  blue: 'var(--td-brand-color)',
  green: 'var(--td-success-color)',
  red: 'var(--td-error-color)',
  orange: 'var(--td-warning-color)',
  purple: 'var(--td-brand-color)',
  cyan: 'var(--td-brand-color)',
};

/** 卡片的跳转目标；`null` = 当前账号没有对应模块的权限，卡片置为不可点 */
interface MetricRoute {
  /** 路由路径 */
  path: string;
  /** 查询参数（审批中心落「我已审批」、任务中心落「我的任务」） */
  query?: Record<string, string>;
  /** 访问所需权限码（与 `sys_menu.auth_code` / 后端 `#[nik::permission]` 逐字一致） */
  permission: string;
}

const METRIC_ROUTES: Record<string, MetricRoute> = {
  // 审批中心：默认就是「待我审批」Tab
  my_todo_count: { path: '/flow/task', permission: 'business:flow:instance:read' },
  // 审批中心「我已审批」Tab（该页认 `?tab=done`，见 flow/task/index.vue）
  my_done_week: {
    path: '/flow/task',
    query: { tab: 'done' },
    permission: 'business:flow:instance:read',
  },
  // 任务中心「我的任务」数据源（该页认 `?mine=1`，见 task/index.vue）
  my_task_today: {
    path: '/task/list',
    query: { mine: '1' },
    permission: 'business:task:read',
  },
  my_overdue: { path: '/task/list', query: { mine: '1' }, permission: 'business:task:read' },
};

/** 有权限才给出跳转目标：没权限的卡片不可点，避免点进去吃 403 */
function routeOf(key: string): MetricRoute | null {
  const target = METRIC_ROUTES[key];
  if (!target) return null;
  return userStore.hasPermission(target.permission) ? target : null;
}

/** 主题键 → 主题色 CSS 变量值 */
function accentVar(card: MetricCard): string {
  // 逾期**只有 > 0** 才用危险色：0 项还标红等于天天报警，看久了就麻木了。
  // 0 时退成次级文字色（与「没有逾期」那句话同一档），图标/数字都跟着变。
  if (card.key === 'my_overdue' && card.value <= 0) {
    return 'var(--td-text-color-secondary)';
  }
  return THEME_VARS[card.theme] ?? 'var(--td-brand-color)';
}

/** 逾期 > 0 才标红（0 时红色会变成假警报，看多了就麻木了） */
function isDanger(card: MetricCard): boolean {
  return card.key === 'my_overdue' && card.value > 0;
}

/**
 * 卡片第三行小字。
 *
 * - 逾期 > 0：「需要处理」；0：「没有逾期」；
 * - 今日任务：有附加数时「已完成 N」，否则给一句口径（今天到期 + 逾期）；
 * - 其它：直接给口径说明的短句，不硬造「较昨日 +2」这类没有数据支撑的对比。
 */
function extraText(card: MetricCard): string {
  if (card.key === 'my_overdue') {
    return card.value > 0 ? '需要处理' : '没有逾期';
  }
  if (card.extra > 0) {
    return `已完成 ${card.extra}`;
  }
  if (card.key === 'my_task_today') {
    return '今天到期 · 含逾期';
  }
  if (card.key === 'my_todo_count') {
    return card.value > 0 ? '点击去审批' : '暂无待办';
  }
  return card.value > 0 ? '本周已审批' : '本周还没审批';
}

/** 点卡片跳对应页面（无权限的卡片是 disabled，不会走到这里） */
function go(key: string): void {
  const target = routeOf(key);
  if (!target) return;
  void router.push({ path: target.path, query: target.query });
}

/**
 * 拉我的指标。
 *
 * 失败按「窄提示 + 重试」处理：首页其余卡片（任务 / 公告 / 审批）不因为这一条接口
 * 抽风而整块变空。失败时**不显示 0** —— 那是把故障伪装成「今天没有待办」。
 */
async function load(): Promise<void> {
  loading.value = true;
  errorText.value = '';
  try {
    const list = await fetchMyMetrics();
    // 防御：后端契约固定 4 条，真回多了也不铺满两行
    cards.value = list.slice(0, 4);
    loaded.value = true;
  } catch (error) {
    cards.value = [];
    errorText.value =
      error instanceof ApiError ? error.message : '我的指标加载失败，请稍后重试';
  } finally {
    loading.value = false;
  }
}

/** 首屏拉一次；失败留重试入口（不自动轮询：这些数字以「打开首页那一刻」为准） */
onMounted(() => {
  void load();
});
</script>

<style scoped lang="less">
@keyframes mm-shimmer {
  from {
    background-position: 100% 0;
  }

  to {
    background-position: 0 0;
  }
}

.mm {
  margin-top: 16px;
}

/* 一行 4 张；<1200px 两列、<768px 单列。内联不给列数，媒体查询才压得住 */
.mm__grid {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 16px;

  @media (max-width: 1199px) {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  @media (max-width: 767px) {
    grid-template-columns: repeat(1, minmax(0, 1fr));
  }
}

/* 整张卡是 button：键盘 Tab 可达、Enter 可跳（不是 div + @click） */
.mm__card {
  position: relative;
  display: flex;
  gap: 14px;
  align-items: center;
  width: 100%;
  padding: 18px 20px;
  font: inherit;
  color: inherit;
  text-align: left;
  cursor: pointer;
  background-color: var(--td-bg-color-container);
  border: 1px solid var(--td-component-stroke);
  border-radius: 12px;
  box-shadow: 0 1px 3px rgb(0 0 0 / 4%);
  transition:
    transform 0.18s ease,
    box-shadow 0.18s ease,
    border-color 0.18s ease;

  &:hover {
    border-color: var(--td-brand-color-light-active);
    box-shadow: 0 8px 20px rgb(17 34 64 / 10%);
    transform: translateY(-2px);
  }

  &:focus-visible {
    outline: 2px solid var(--td-brand-color);
    outline-offset: 2px;
  }

  /* 逾期 > 0：左侧 2px 红边（与数字红色同一语义，扫一眼就能定位） */
  &.is-danger {
    border-left: 2px solid var(--td-error-color);
  }

  /* 没权限：不给悬浮反馈，也不显示手型，避免「看着能点、点了 403」 */
  &.is-static {
    cursor: default;

    &:hover {
      border-color: var(--td-component-stroke);
      box-shadow: 0 1px 3px rgb(0 0 0 / 4%);
      transform: none;
    }
  }
}

.mm__icon {
  display: inline-flex;
  flex: none;
  align-items: center;
  justify-content: center;
  width: 44px;
  height: 44px;
  font-size: 22px;
  color: var(--mm-accent, var(--td-brand-color));
  background-color: var(--td-bg-color-container-hover);
  border-radius: 12px;
}

.mm__body {
  display: flex;
  flex: 1 1 auto;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.mm__value {
  font-size: 30px;
  font-weight: 600;
  line-height: 1.1;
  color: var(--mm-accent, var(--td-brand-color));
  font-variant-numeric: tabular-nums;
  letter-spacing: -0.5px;
}

/* 逾期 > 0：数字必须是红色（卡片已把 --mm-accent 设成 danger 主题，这里再钉一次） */
.mm__card.is-danger .mm__value {
  color: var(--td-error-color);
}

.mm__label {
  overflow: hidden;
  font-size: 13px;
  color: var(--td-text-color-secondary);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.mm__unit {
  margin-left: 3px;
  font-size: 12px;
  font-weight: 400;
  color: var(--td-text-color-placeholder);
  letter-spacing: 0;
}

.mm__extra {
  overflow: hidden;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* 骨架：同尺寸占位 + 流光，避免首屏高度跳变 */
.mm__skeleton {
  height: 84px;
  background: linear-gradient(
    90deg,
    var(--td-bg-color-container-hover) 25%,
    var(--td-bg-color-secondarycontainer) 37%,
    var(--td-bg-color-container-hover) 63%
  );
  background-size: 400% 100%;
  border-radius: 12px;
  animation: mm-shimmer 1.4s ease infinite;
}

.mm__error {
  display: flex;
  gap: 6px;
  align-items: center;
  margin-top: 8px;
  padding: 6px 10px;
  font-size: 12px;
  color: var(--td-error-color);
  background-color: var(--td-error-color-1);
  border-radius: var(--td-radius-default);
}
</style>

<template>
  <t-card :bordered="false" class="health">
    <template #title>
      <span class="health__title">
        系统健康
        <span v-if="collectedAt" class="health__collected">更新于 {{ collectedAt }}</span>
      </span>
    </template>
    <template #actions>
      <div class="health__actions">
        <t-link theme="primary" hover="color" :disabled="loading" @click="load()">
          <t-icon name="refresh" />
        </t-link>
        <t-link theme="primary" hover="color" @click="goDetail">
          查看详情
          <t-icon name="chevron-right" />
        </t-link>
      </div>
    </template>

    <!-- 首屏骨架：4 个同尺寸占位，数据回来时不跳版 -->
    <div v-if="loading && !data" class="health__grid">
      <div v-for="n in 4" :key="`sk-${n}`" class="health__item health__item--skeleton">
        <span class="health__skeleton health__skeleton--label" />
        <span class="health__skeleton health__skeleton--value" />
        <span class="health__skeleton health__skeleton--foot" />
      </div>
    </div>

    <div v-else-if="error" class="health__error">
      <t-icon name="error-circle" />
      <span>{{ error }}</span>
      <t-link theme="primary" hover="color" @click="load()">重试</t-link>
    </div>

    <div v-else class="health__grid">
      <div v-for="item in items" :key="item.key" class="health__item">
        <span class="health__label">
          <t-icon class="health__icon" :name="item.icon" />
          {{ item.label }}
        </span>
        <span class="health__value" :class="`health__value--${item.tone}`">
          {{ item.value }}
          <span v-if="item.unit" class="health__unit">{{ item.unit }}</span>
        </span>
        <span class="health__foot" :title="item.foot">{{ item.foot }}</span>
      </div>
    </div>
  </t-card>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { useRouter } from 'vue-router';
import { fetchServerStatus, type ServerStatusRes } from '@/api/business/monitor-api';
import { ApiError } from '@/api/request';

/** 服务器状态页（HOME-3）；菜单与路由都只授超管，本卡也只在超管「系统信息」Tab 渲染 */
const SERVER_STATUS_PATH = '/system/server-status';

/** 百分比阈值：与「服务器状态」页同一口径（≥60% 警告、≥80% 异常） */
const WARN_PERCENT = 60;
const DANGER_PERCENT = 80;

/** 四个指标中的一个 */
interface HealthItem {
  /** 指标键 */
  key: string;
  /** 指标名 */
  label: string;
  /** 图标名 */
  icon: string;
  /** 主体数值（含 % 或分数） */
  value: string;
  /** 数值后缀单位（内存/磁盘的 % 已在 value 里，这里留空） */
  unit: string;
  /** 脚注：核数 / 已用容量 / 异常项 */
  foot: string;
  /** 数值配色档：ok / warn / danger / muted */
  tone: 'ok' | 'warn' | 'danger' | 'muted';
}

const router = useRouter();

/** 状态数据（未加载或被清空时为 null） */
const data = ref<ServerStatusRes | null>(null);
/** 首屏加载中（有旧数据时静默刷新，不闪骨架） */
const loading = ref(false);
/** 加载失败提示 */
const error = ref('');

/** 百分比取值 → 三档色（主题色 / 琥珀 / 红），与服务器状态页同一函数语义 */
function toneOf(percent: number): 'ok' | 'warn' | 'danger' {
  if (percent >= DANGER_PERCENT) return 'danger';
  if (percent >= WARN_PERCENT) return 'warn';
  return 'ok';
}

/** 服务在线与否：`up` 才算在线；`not_installed` / `down` 都算不在线（文案各自区分） */
function isUp(state?: string): boolean {
  return (state || '').toUpperCase() === 'UP';
}

/** 采集时间只到分钟（`11-09 22:31`）——秒级精度在这个卡片里只是噪声 */
const collectedAt = computed(() => {
  const raw = data.value?.collected_at;
  if (!raw) return '';
  const d = new Date(raw);
  if (Number.isNaN(d.getTime())) return '';
  const pad = (v: number) => String(v).padStart(2, '0');
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
});

/**
 * 四张指标。
 *
 * 「服务」只数**后端真正探得动**的三项依赖（数据库 / Redis / Hermes Agent）：
 * 设计稿写的是 4/5，但接口里没有第 4、5 项的健康字段，凭空补齐就是假数据。
 * Hermes 在未部署时是 `not_installed`（正常状态），不算故障，只在脚注里写明。
 */
const items = computed<HealthItem[]>(() => {
  const res = data.value;
  if (!res) return [];

  const cpu = res.cpu?.percent ?? 0;
  const mem = res.memory?.percent ?? 0;
  const disk = res.disk?.percent ?? 0;

  const probes = [
    { name: '数据库', state: res.database?.state },
    { name: 'Redis', state: res.redis?.state },
    { name: 'Hermes Agent', state: res.hermes?.state },
  ];
  const upCount = probes.filter((probe) => isUp(probe.state)).length;
  // 数据库 / Redis 掉线才是真故障；Hermes 未部署只是「没装」，不能一起判红
  const coreDown = probes.slice(0, 2).filter((probe) => !isUp(probe.state));
  const notUp = probes.filter((probe) => !isUp(probe.state));
  const serviceTone: HealthItem['tone'] = coreDown.length ? 'danger' : notUp.length ? 'muted' : 'ok';

  return [
    {
      key: 'cpu',
      label: 'CPU',
      icon: 'chart-pie',
      value: `${cpu.toFixed(1)}%`,
      unit: '',
      foot: `${res.cpu?.cores ?? 0} 逻辑核心`,
      tone: toneOf(cpu),
    },
    {
      key: 'memory',
      label: '内存',
      icon: 'chart-ring',
      value: `${mem.toFixed(1)}%`,
      unit: '',
      foot: `${res.memory?.used_text ?? '-'} / ${res.memory?.total_text ?? '-'}`,
      tone: toneOf(mem),
    },
    {
      key: 'disk',
      label: '磁盘',
      icon: 'chart',
      value: `${disk.toFixed(1)}%`,
      unit: '',
      foot: `${res.disk?.used_text ?? '-'} / ${res.disk?.total_text ?? '-'}`,
      tone: toneOf(disk),
    },
    {
      key: 'service',
      label: '服务',
      icon: 'server',
      value: `${upCount}/${probes.length}`,
      unit: '',
      foot: notUp.length
        ? notUp.map((probe) => `${probe.name}${(probe.state || '').toUpperCase() === 'NOT_INSTALLED' ? ' 未安装' : ' 不可用'}`).join(' · ')
        : '全部在线',
      tone: serviceTone,
    },
  ];
});

/** 详情页跳转（`/system/server-status`，路由与菜单都只授超管） */
function goDetail(): void {
  void router.push(SERVER_STATUS_PATH);
}

/**
 * 拉服务器状态。
 *
 * 权限码只授超管 —— 本组件只在超管「系统信息」Tab 里挂载（父级 `v-if` 控制），
 * 非超管连请求都不会发出，控制台不会出现 403。
 */
async function load(): Promise<void> {
  loading.value = true;
  error.value = '';
  try {
    data.value = await fetchServerStatus();
  } catch (e) {
    // 保留旧数据：刷新失败时显示旧快照 + 错误条，比清空更有用
    error.value = e instanceof ApiError ? e.message : '服务器状态加载失败';
  } finally {
    loading.value = false;
  }
}

onMounted(() => {
  void load();
});
</script>

<style scoped lang="less">
.health {
  &__title {
    display: inline-flex;
    gap: 8px;
    align-items: baseline;
  }

  // 「更新于 xx:xx」比标题小一号、浅一档：是注脚不是标题
  &__collected {
    font-size: 12px;
    font-weight: 400;
    color: var(--td-text-color-placeholder);
    font-variant-numeric: tabular-nums;
  }

  &__actions {
    display: flex;
    gap: 12px;
    align-items: center;
    font-size: 13px;
  }

  // 4 个指标横排；窄屏两列、超窄一列（内容单列，不横向滚动）
  &__grid {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 12px;

    @media (max-width: 1199px) {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }

    @media (max-width: 599px) {
      grid-template-columns: minmax(0, 1fr);
    }
  }

  &__item {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
    padding: 14px 16px;
    background-color: var(--td-bg-color-container-hover);
    border: 1px solid transparent;
    border-radius: 10px;
    transition: border-color 0.2s ease, transform 0.2s ease;

    &:hover {
      border-color: var(--td-brand-color-light-active);
      transform: translateY(-1px);
    }

    &--skeleton {
      gap: 10px;
    }
  }

  &__label {
    display: inline-flex;
    gap: 6px;
    align-items: center;
    font-size: 13px;
    color: var(--td-text-color-secondary);
  }

  &__icon {
    font-size: 14px;
    color: var(--td-text-color-placeholder);
  }

  &__value {
    font-size: 26px;
    font-weight: 600;
    line-height: 1.2;
    color: var(--td-text-color-primary);
    font-variant-numeric: tabular-nums;

    // 阈值配色沿用语义色变量（配色方案切换时自动跟随）
    &--ok {
      color: var(--td-brand-color);
    }

    &--warn {
      color: var(--td-warning-color);
    }

    &--danger {
      color: var(--td-error-color);
    }

    &--muted {
      color: var(--td-text-color-secondary);
    }
  }

  &__unit {
    margin-left: 2px;
    font-size: 13px;
    font-weight: 400;
    color: var(--td-text-color-placeholder);
  }

  &__foot {
    overflow: hidden;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
    text-overflow: ellipsis;
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }

  &__skeleton {
    display: block;
    background: linear-gradient(
      90deg,
      var(--td-bg-color-container-hover) 25%,
      var(--td-bg-color-secondarycontainer) 37%,
      var(--td-bg-color-container-hover) 63%
    );
    background-size: 400% 100%;
    border-radius: var(--td-radius-default);

    &--label {
      width: 46px;
      height: 12px;
    }

    &--value {
      width: 78px;
      height: 24px;
    }

    &--foot {
      width: 64%;
      height: 11px;
    }
  }

  &__error {
    display: flex;
    gap: 8px;
    align-items: center;
    font-size: 13px;
    color: var(--td-error-color);
  }
}
</style>

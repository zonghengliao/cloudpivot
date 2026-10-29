<template>
  <div class="gantt-page">
    <!-- 工具条：时间窗 / 缩放 / 筛选（**只读**，没有任何拖拽入口） -->
    <t-card :bordered="false" class="toolbar-card">
      <t-row :gutter="[16, 12]" align="middle">
        <t-col :span="4">
          <t-space align="center">
            <t-button variant="text" @click="goList">
              <template #icon><t-icon name="chevron-left" /></template>
              返回列表
            </t-button>
            <span class="toolbar__title">甘特图</span>
            <t-tag size="small" variant="light-outline" theme="default">只读</t-tag>
          </t-space>
        </t-col>
        <t-col :span="4">
          <t-space align="center">
            <t-button size="small" variant="outline" @click="shiftWindow(-1)">上一屏</t-button>
            <t-button size="small" variant="outline" @click="goToday">今天</t-button>
            <t-button size="small" variant="outline" @click="shiftWindow(1)">下一屏</t-button>
          </t-space>
        </t-col>
        <t-col :span="4">
          <t-date-range-picker
            v-model="windowRange"
            size="small"
            clearable
            format="YYYY-MM-DD"
            placeholder="时间窗（默认今天起 30 天，上限 180 天）"
          />
        </t-col>
        <t-col :span="4">
          <t-space align="center">
            <t-radio-group v-model="zoom" size="small" variant="default-filled">
              <t-radio-button value="day">日</t-radio-button>
              <t-radio-button value="week">周</t-radio-button>
            </t-radio-group>
            <t-radio-group v-model="scope" size="small" variant="default-filled">
              <t-radio-button value="all">全部可见</t-radio-button>
              <t-radio-button value="my">只看我的</t-radio-button>
            </t-radio-group>
            <t-button size="small" variant="outline" :loading="loading" @click="load">
              <template #icon><t-icon name="refresh" /></template>
              刷新
            </t-button>
          </t-space>
        </t-col>
      </t-row>
      <div v-if="data" class="toolbar__meta">
        窗口 {{ data.range.start }} ~ {{ data.range.end }}（{{ data.range.days }} 天） · 任务
        {{ data.tasks.length }} 条 · 依赖 {{ data.dependencies.length }} 条
        <span v-if="data.truncated" class="toolbar__warn">· 任务数超过 500，已截断，请缩小区间</span>
        <span v-if="data.member_scope_truncated" class="toolbar__warn">
          · 我参与的任务过多，本视图只按数据范围显示
        </span>
      </div>
    </t-card>

    <!-- 错误态：可重试，不白屏 -->
    <t-card v-if="errorMessage" :bordered="false" class="body-card">
      <t-alert theme="error" :message="errorMessage">
        <template #operation>
          <t-button size="small" variant="outline" @click="load">重试</t-button>
        </template>
      </t-alert>
    </t-card>

    <t-card v-else :bordered="false" class="body-card">
      <t-loading :loading="loading" size="small">
        <!-- 空态：没有任何可排期任务（都没有 start_date / due_date） -->
        <div v-if="!loading && visibleTasks.length === 0" class="gantt-empty">
          <t-icon name="calendar" size="32px" class="gantt-empty__icon" />
          <p class="gantt-empty__text">
            这个时间窗内没有可排期的任务（任务需要有「计划开始」或「截止日期」才能画到甘特图上）
          </p>
          <t-button size="small" variant="outline" @click="goToday">回到今天</t-button>
        </div>

        <!-- 甘特主体：**纯 SVG 手绘**（不引任何甘特库；拖拽属 T7，本页不绑任何 pointer 事件） -->
        <div v-else class="gantt-scroll">
          <svg
            class="gantt-svg"
            :width="svgWidth"
            :height="svgHeight"
            :viewBox="`0 0 ${svgWidth} ${svgHeight}`"
            role="img"
            aria-label="任务甘特图（只读）"
          >
            <defs>
              <!-- 箭头：依赖线末端 -->
              <marker
                id="gantt-arrow"
                viewBox="0 0 10 10"
                refX="9"
                refY="5"
                markerWidth="6"
                markerHeight="6"
                orient="auto-start-reverse"
              >
                <path d="M 0 0 L 10 5 L 0 10 z" :fill="cssVar('--td-brand-color', '#0052d9')" />
              </marker>
              <!-- 被阻塞任务的斜纹底（核心设计 §五：斜纹=被阻塞） -->
              <pattern id="gantt-blocked" width="6" height="6" patternUnits="userSpaceOnUse">
                <rect width="6" height="6" :fill="cssVar('--td-bg-color-container', '#ffffff')" />
                <path
                  d="M -1 1 L 1 -1 M 0 6 L 6 0 M 5 7 L 7 5"
                  :stroke="cssVar('--td-component-stroke', '#dcdcdc')"
                  stroke-width="1"
                />
              </pattern>
            </defs>

            <!-- ① 表头：日期刻度（日 / 周两档） -->
            <g class="gantt-head">
              <rect x="0" y="0" :width="svgWidth" :height="HEAD_H" :fill="cssVar('--td-bg-color-secondarycontainer', '#f3f3f3')" />
              <text :x="LEFT_W - 10" :y="HEAD_H / 2 + 4" text-anchor="end" class="gantt-head__label">
                任务（{{ visibleTasks.length }}）
              </text>
              <g v-for="tick in ticks" :key="tick.key">
                <line
                  :x1="tick.x"
                  :y1="HEAD_H - 8"
                  :x2="tick.x"
                  :y2="svgHeight"
                  :stroke="cssVar('--td-component-stroke', '#e7e7e7')"
                  stroke-width="1"
                />
                <text :x="tick.x + 4" :y="HEAD_H / 2 + 4" class="gantt-head__tick">{{ tick.label }}</text>
              </g>
            </g>

            <!-- ② 左侧任务名 + 行分隔线（可折叠子任务） -->
            <g v-for="(row, index) in rows" :key="row.id">
              <rect
                :x="0"
                :y="rowTop(index)"
                :width="LEFT_W"
                :height="ROW_H"
                :fill="index % 2 === 0 ? 'transparent' : cssVar('--td-bg-color-container-hover', '#fafafa')"
              />
              <line
                x1="0"
                :y1="rowTop(index) + ROW_H"
                :x2="svgWidth"
                :y2="rowTop(index) + ROW_H"
                :stroke="cssVar('--td-component-stroke', '#f0f0f0')"
                stroke-width="1"
              />
              <!-- 折叠箭头（只有父任务才有孩子） -->
              <text
                v-if="row.childCount > 0"
                :x="8 + row.depth * 12"
                :y="rowTop(index) + ROW_H / 2 + 4"
                class="gantt-toggle"
                @click="toggle(row.id)"
              >
                {{ collapsed.has(row.id) ? '▸' : '▾' }}
              </text>
              <circle
                :cx="24 + row.depth * 12"
                :cy="rowTop(index) + ROW_H / 2"
                r="3.5"
                :fill="statusColor(row.task.status)"
              />
              <text
                :x="34 + row.depth * 12"
                :y="rowTop(index) + ROW_H / 2 + 4"
                class="gantt-name"
                :class="{ 'gantt-name--done': row.task.status === 'DONE' }"
              >
                {{ clip(row.task.title, 16 - row.depth * 2) }}
              </text>
            </g>

            <!-- ③ 任务条：颜色按状态、内条按进度、逾期描红、被阻塞加斜纹 -->
            <g v-for="(row, index) in rows" :key="`bar-${row.id}`">
              <template v-if="barOf(row)">
                <rect
                  :x="barOf(row)!.x"
                  :y="rowTop(index) + 8"
                  :width="barOf(row)!.width"
                  :height="BAR_H"
                  rx="4"
                  :fill="statusColor(row.task.status)"
                  fill-opacity="0.28"
                  :stroke="row.task.overdue ? cssVar('--td-error-color', '#d54941') : statusColor(row.task.status)"
                  :stroke-width="row.task.overdue ? 2 : 1"
                  class="gantt-bar"
                  @click="openDetail(row.task.id)"
                >
                  <title>{{ tooltipOf(row.task) }}</title>
                </rect>
                <!-- 被阻塞：斜纹覆盖（只覆盖未完成的部分，代表「还不能开始」） -->
                <rect
                  v-if="row.task.blocked"
                  :x="barOf(row)!.x"
                  :y="rowTop(index) + 8"
                  :width="barOf(row)!.width"
                  :height="BAR_H"
                  rx="4"
                  fill="url(#gantt-blocked)"
                  fill-opacity="0.55"
                  class="gantt-bar"
                  @click="openDetail(row.task.id)"
                />
                <!-- 进度内条（progress% 宽度） -->
                <rect
                  v-if="row.task.progress > 0"
                  :x="barOf(row)!.x"
                  :y="rowTop(index) + 8 + BAR_H - 5"
                  :width="Math.max(2, (barOf(row)!.width * row.task.progress) / 100)"
                  :height="5"
                  rx="2"
                  :fill="statusColor(row.task.status)"
                  class="gantt-bar"
                  @click="openDetail(row.task.id)"
                />
              </template>
            </g>

            <!-- ④ 依赖线：前置结束 → 后继开始（折线 + 箭头） -->
            <g>
              <polyline
                v-for="link in links"
                :key="`${link.from}-${link.to}`"
                :points="link.points"
                fill="none"
                :stroke="cssVar('--td-brand-color', '#0052d9')"
                stroke-width="1.2"
                stroke-opacity="0.75"
                marker-end="url(#gantt-arrow)"
              />
            </g>

            <!-- ⑤ 今天竖线（醒目色；今天不在窗口内时不画） -->
            <g v-if="todayX !== null">
              <line
                :x1="todayX"
                :y1="HEAD_H - 6"
                :x2="todayX"
                :y2="svgHeight"
                :stroke="cssVar('--td-error-color', '#d54941')"
                stroke-width="2"
                stroke-dasharray="4 3"
              />
              <text :x="todayX + 4" :y="HEAD_H - 12" class="gantt-today">今天</text>
            </g>
          </svg>
        </div>
      </t-loading>
    </t-card>

    <!-- 点任务条 → T6a 的详情抽屉（同一份组件，不重复实现） -->
    <TaskDetail v-model:visible="detailVisible" :task-id="detailTaskId" @deleted="load" />
  </div>
</template>

<script setup lang="ts">
/**
 * 甘特图（批次 C+ · T4，**只读**）。
 *
 * 依据：`docs/批次C+-工作任务管理-核心设计.md` §五（甘特图：自绘 SVG、任务条、
 * 依赖线、今天线、逾期标红、本版只做「看」）、`docs/云枢设计修正补丁-20261029.md`
 * 修正 10（拖拽改期属 T7，且必须重跑依赖校验）。
 *
 * # 三条实现纪律
 *
 * 1. **纯 SVG 手绘**：不引第三方甘特库（D8 已定）。所有几何量都在本文件算，
 *    但「日期 → 天数偏移」由**后端**给（`task.bar.offset_days / length_days`）——
 *    前后端各算一遍日期差，迟早出现「差一天」。
 * 2. **只读**：不绑任何 `pointerdown` / `mousedown` / `drag*`，也不写 `cursor: grab`。
 *    点击任务条只做一件事：打开详情抽屉。T7 加拖拽时**必须**在改期后重跑依赖校验。
 * 3. **颜色只走主题变量**（`--td-*`）：状态色取自 `constants/status-map.ts` 的
 *    `dotVar`，与列表页 / 详情页的徽标同一份来源；配色方案切换与暗色模式自动跟随。
 */
import { computed, onMounted, ref, watch } from 'vue';
import { useRouter } from 'vue-router';
import dayjs from 'dayjs';
import TaskDetail from './detail.vue';
import { ApiError } from '@/api/request';
import { fetchGantt, type GanttTask, type TaskGantt } from '@/api/business/task-api';
import { statusMeta } from '@/constants/status-map';

/** 布局常量（像素）：左侧任务名宽度、行高、表头高度、任务条高度 */
const LEFT_W = 280;
const ROW_H = 34;
const HEAD_H = 46;
const BAR_H = 18;

/** 缩放：每天多少像素（日 / 周两档） */
const DAY_WIDTH: Record<'day' | 'week', number> = { day: 30, week: 12 };

const router = useRouter();

/** 甘特数据 */
const data = ref<TaskGantt | null>(null);
/** 加载中 */
const loading = ref(false);
/** 错误文案（非空即渲染错误态） */
const errorMessage = ref('');
/** 缩放档 */
const zoom = ref<'day' | 'week'>('day');
/** 筛选：全部可见 / 只看我的 */
const scope = ref<'all' | 'my'>('all');
/** 显式时间窗（清空 → 后端默认今天起 30 天；与列表页的 date-range 同样用 string[]） */
const windowRange = ref<string[]>([]);
/** 折叠的父任务 id */
const collapsed = ref<Set<string>>(new Set());
/** 详情抽屉 */
const detailVisible = ref(false);
/** 详情抽屉的任务 id */
const detailTaskId = ref<string | null>(null);

/** 每天像素宽度 */
const dayWidth = computed(() => DAY_WIDTH[zoom.value]);
/** 窗口天数（后端已保证 1–180） */
const dayCount = computed(() => data.value?.range.days ?? 0);
/** SVG 宽度 = 左侧 + 时间轴 */
const svgWidth = computed(() => LEFT_W + dayCount.value * dayWidth.value);

/** 行（父任务在前、子任务紧随；父任务折叠时子任务不出现） */
interface GanttRow {
  id: string;
  task: GanttTask;
  depth: number;
  childCount: number;
}

/** 父子分组：按后端给的 parent_id 组树（深度最多 2 层，T1 的约束） */
const allRows = computed<GanttRow[]>(() => {
  const tasks = data.value?.tasks ?? [];
  const byId = new Map(tasks.map((task) => [task.id, task]));
  const childrenOf = new Map<string, GanttTask[]>();
  for (const task of tasks) {
    if (task.parent_id && byId.has(task.parent_id)) {
      const list = childrenOf.get(task.parent_id) ?? [];
      list.push(task);
      childrenOf.set(task.parent_id, list);
    }
  }
  const rows: GanttRow[] = [];
  for (const task of tasks) {
    // 子任务由父任务带出来，这里跳过（父任务不可见时它自己就是根，正常显示）
    if (task.parent_id && byId.has(task.parent_id)) {
      continue;
    }
    const kids = childrenOf.get(task.id) ?? [];
    rows.push({ id: task.id, task, depth: 0, childCount: kids.length });
    if (collapsed.value.has(task.id)) {
      continue;
    }
    for (const child of kids) {
      rows.push({ id: child.id, task: child, depth: 1, childCount: 0 });
    }
  }
  return rows;
});

/** 实际渲染的行（折叠已在上一步处理） */
const rows = allRows;
/** 可见任务数（空态判定用） */
const visibleTasks = computed(() => rows.value);

/** SVG 高度（表头 + 行 + 底部留白） */
const svgHeight = computed(() => HEAD_H + rows.value.length * ROW_H + 12);

/** 行顶 y 坐标 */
function rowTop(index: number): number {
  return HEAD_H + index * ROW_H;
}

/** task_id → 行序号（依赖线定位用） */
const rowIndexById = computed(() => {
  const map = new Map<string, number>();
  rows.value.forEach((row, index) => map.set(row.id, index));
  return map;
});

/** 任务条几何（x = 左侧 + 偏移天 × 列宽） */
function barOf(row: GanttRow): { x: number; width: number } | null {
  const bar = row.task.bar;
  if (!bar) {
    return null;
  }
  return {
    x: LEFT_W + bar.offset_days * dayWidth.value,
    width: Math.max(6, bar.length_days * dayWidth.value - 2),
  };
}

/** 依赖线：前置条右端 → 后继条左端（折线，拐点在中缝） */
const links = computed(() => {
  const out: { from: string; to: string; points: string }[] = [];
  for (const dep of data.value?.dependencies ?? []) {
    const predIndex = rowIndexById.value.get(dep.from);
    const succIndex = rowIndexById.value.get(dep.to);
    if (predIndex === undefined || succIndex === undefined) {
      continue; // 一端被折叠 / 不在窗口内 → 不画
    }
    const pred = barOf(rows.value[predIndex] ?? { id: '', task: {} as GanttTask, depth: 0, childCount: 0 });
    const succ = barOf(rows.value[succIndex] ?? { id: '', task: {} as GanttTask, depth: 0, childCount: 0 });
    if (!pred || !succ) {
      continue;
    }
    const x1 = pred.x + pred.width;
    const y1 = rowTop(predIndex) + 8 + BAR_H / 2;
    const x2 = succ.x;
    const y2 = rowTop(succIndex) + 8 + BAR_H / 2;
    // 中缝：后继条在左侧时（画不出标准 FS 形状）也保证线不穿过条形
    const midX = x2 > x1 ? x1 + 10 : x2 - 10;
    const points = `${x1},${y1} ${midX},${y1} ${midX},${y2} ${Math.max(LEFT_W, x2 - 4)},${y2}`;
    out.push({ from: dep.from, to: dep.to, points });
  }
  return out;
});

/** 日期刻度：日档每天一根、周档每周一根（周一） */
const ticks = computed(() => {
  const range = data.value?.range;
  if (!range) {
    return [];
  }
  const out: { key: string; label: string; x: number }[] = [];
  const start = dayjs(range.start);
  for (let index = 0; index < range.days; index += 1) {
    const day = start.add(index, 'day');
    const isWeekStart = day.day() === 1;
    const show = zoom.value === 'day' ? day.date() === 1 || index === 0 || day.day() === 1 : isWeekStart;
    if (!show) {
      continue;
    }
    out.push({
      key: day.format('YYYY-MM-DD'),
      label: zoom.value === 'day' ? day.format('MM-DD') : day.format('MM-DD'),
      x: LEFT_W + index * dayWidth.value,
    });
  }
  return out;
});

/** 今天竖线的 x 坐标（后端给偏移，今天不在窗口内 → null） */
const todayX = computed(() => {
  const offset = data.value?.range.today_offset;
  if (offset === null || offset === undefined) {
    return null;
  }
  return LEFT_W + offset * dayWidth.value + dayWidth.value / 2;
});

/** 读主题变量（SSR / 未注入时给 fallback，避免整块变透明） */
function cssVar(name: string, fallback: string): string {
  if (typeof window === 'undefined') {
    return fallback;
  }
  const value = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return value || fallback;
}

/** 状态色（唯一来源：status-map 的 dotVar） */
function statusColor(status: string): string {
  const meta = statusMeta('task', status);
  return cssVar(meta.dotVar.replace(/^var\(/, '').replace(/\)$/, ''), '#0052d9');
}

/** 条形 tooltip（标题 / 编号 / 日期 / 进度 / 负责人 / 状态） */
function tooltipOf(task: GanttTask): string {
  const range = `${task.start_date ?? '—'} ~ ${task.due_date ?? '—'}`;
  const flags = [
    task.overdue ? '已逾期' : '',
    task.blocked ? '被前置阻塞' : '',
    statusMeta('task', task.status).label,
  ]
    .filter(Boolean)
    .join(' / ');
  return `${task.task_no} ${task.title}\n${range} · 进度 ${task.progress}%\n${task.owner_name || '—'} · ${flags}`;
}

/** 标题截断（SVG 里的 text 不会自动换行） */
function clip(text: string, limit: number): string {
  const max = Math.max(4, limit);
  return text.length > max ? `${text.slice(0, max)}…` : text;
}

/** 折叠 / 展开父任务 */
function toggle(id: string): void {
  const next = new Set(collapsed.value);
  if (next.has(id)) {
    next.delete(id);
  } else {
    next.add(id);
  }
  collapsed.value = next;
}

/** 打开详情抽屉（点任务条唯一的交互） */
function openDetail(id: string): void {
  detailTaskId.value = id;
  detailVisible.value = true;
}

/** 回列表页 */
function goList(): void {
  void router.push('/task');
}

/** 把时间窗推回默认（今天起 30 天） */
function goToday(): void {
  windowRange.value = [];
  void load();
}

/** 平移窗口（上一屏 / 下一屏：按当前窗口长度移动） */
function shiftWindow(direction: 1 | -1): void {
  const range = data.value?.range;
  if (!range) {
    return;
  }
  const days = range.days;
  const start = dayjs(range.start).add(direction * days, 'day');
  const end = dayjs(range.end).add(direction * days, 'day');
  windowRange.value = [start.format('YYYY-MM-DD'), end.format('YYYY-MM-DD')];
  void load();
}

/** 拉数据（时间窗 / scope 变化都会重拉；缩放只影响像素不影响查询） */
async function load(): Promise<void> {
  loading.value = true;
  errorMessage.value = '';
  const [start, end] = windowRange.value;
  try {
    data.value = await fetchGantt({
      scope: scope.value,
      start: start || undefined,
      end: end || undefined,
    });
    // 窗口换了之后折叠状态可能指向已经不在数据里的任务，顺手瘦身
    const alive = new Set((data.value?.tasks ?? []).map((task) => task.id));
    const next = new Set([...collapsed.value].filter((id) => alive.has(id)));
    if (next.size !== collapsed.value.size) {
      collapsed.value = next;
    }
  } catch (error) {
    data.value = null;
    errorMessage.value =
      error instanceof ApiError
        ? error.message
        : error instanceof Error
          ? error.message
          : '甘特图加载失败';
  } finally {
    loading.value = false;
  }
}

// 筛选 / 时间窗变化 → 重新查询（缩放只改像素，不重新查询）
watch([scope, windowRange], () => {
  void load();
});
// 缩放变化时保持折叠状态，仅重算像素（computed 自动跟随，这里只留一处显式说明）
watch(zoom, () => {
  /* 无需请求：几何量全部由 computed 推导 */
});

onMounted(() => {
  void load();
});
</script>

<style scoped lang="less">
.gantt-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.toolbar-card,
.body-card {
  border-radius: 8px;
}

.toolbar__title {
  font-size: 16px;
  font-weight: 600;
  color: var(--td-text-color-primary, #1f2329);
}

.toolbar__meta {
  margin-top: 8px;
  font-size: 12px;
  color: var(--td-text-color-secondary, #666);
}

.toolbar__warn {
  color: var(--td-warning-color, #e37318);
}

.gantt-scroll {
  overflow-x: auto;
  overflow-y: auto;
  max-height: 68vh;
}

.gantt-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  padding: 48px 0;

  &__icon {
    color: var(--td-text-color-placeholder, #bbb);
  }

  &__text {
    margin: 0;
    font-size: 13px;
    color: var(--td-text-color-secondary, #666);
  }
}

.gantt-svg {
  display: block;
  // 只读：不写 cursor: grab / move（拖拽属 T7），点击是「打开详情」
  cursor: default;
  user-select: none;
}

.gantt-head__label {
  font-size: 12px;
  fill: var(--td-text-color-secondary, #666);
}

.gantt-head__tick {
  font-size: 11px;
  fill: var(--td-text-color-secondary, #888);
}

.gantt-name {
  font-size: 12px;
  fill: var(--td-text-color-primary, #333);

  &--done {
    fill: var(--td-text-color-placeholder, #999);
    text-decoration: line-through;
  }
}

.gantt-toggle {
  font-size: 11px;
  fill: var(--td-text-color-secondary, #666);
  cursor: pointer;
}

.gantt-bar {
  cursor: pointer;
}

.gantt-today {
  font-size: 11px;
  fill: var(--td-error-color, #d54941);
}
</style>

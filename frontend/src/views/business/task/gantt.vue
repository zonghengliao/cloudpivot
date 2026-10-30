<template>
  <div class="gantt-page">
    <!-- 查询框：负责人 / 项目 / 时间窗 / 屏幕切换 / 缩放 / 可见范围（与 index.vue 分工一致） -->
    <t-card :bordered="false" class="filter-card">
      <t-row :gutter="[16, 12]">
        <t-col :span="6">
          <UserSelect v-model="filterAssignee" placeholder="负责人" />
        </t-col>
        <t-col :span="6">
          <t-select
            v-model="filterProject"
            :options="projectOptions"
            :loading="projectLoading"
            placeholder="所属项目"
            clearable
            filterable
          />
        </t-col>
        <t-col :span="7">
          <t-date-range-picker
            v-model="windowRange"
            clearable
            format="YYYY-MM-DD"
            placeholder="时间窗（默认今天起 30 天，上限 180 天）"
          />
        </t-col>
        <t-col :span="5">
          <t-space>
            <t-button theme="primary" @click="handleFilter">
              <template #icon><t-icon name="search" /></template>
              查询
            </t-button>
            <t-button variant="outline" @click="handleFilterReset">重置</t-button>
          </t-space>
        </t-col>
        <t-col :span="24">
          <div v-if="data" class="filter-meta">
            窗口 {{ data.range.start }} ~ {{ data.range.end }}（{{ data.range.days }} 天）
            · 任务 {{ data.tasks.length }} 条 · 依赖 {{ data.dependencies.length }} 条
            <span class="filter-meta__tag">可拖拽改期</span>
            <span v-if="data.truncated" class="filter-meta__warn">· 超过 500 条已截断，请缩小区间</span>
            <span v-if="data.member_scope_truncated" class="filter-meta__warn">· 我参与的任务过多，只按数据范围显示</span>
          </div>
        </t-col>
      </t-row>
    </t-card>

    <!-- 工具条：三视图切换 + 主操作（与 index.vue 完全一致） -->
    <t-card :bordered="false" class="toolbar-card">
      <div class="toolbar">
        <t-space align="center">
          <t-tabs :model-value="'gantt'" class="view-switch" :space-evenly="false" @change="handleViewChange">
            <t-tab-panel value="list">
              <template #label>
                <span class="view-switch__label"><t-icon name="view-list" /> 任务列表</span>
              </template>
            </t-tab-panel>
            <t-tab-panel value="board">
              <template #label>
                <span class="view-switch__label"><t-icon name="view-module" /> 任务看板</span>
              </template>
            </t-tab-panel>
            <t-tab-panel value="gantt">
              <template #label>
                <span class="view-switch__label"><t-icon name="view-gantt" /> 甘特图</span>
              </template>
            </t-tab-panel>
            <t-tab-panel value="burndown">
              <template #label>
                <span class="view-switch__label"><t-icon name="chart-line" /> 燃尽图</span>
              </template>
            </t-tab-panel>
          </t-tabs>
          <t-button v-privilege="[PERM.taskCreate]" theme="primary" size="small" @click="openCreate">
            <template #icon><t-icon name="add" /></template>
            新建任务
          </t-button>
          <t-button variant="outline" size="small" :loading="loading" @click="load">
            <template #icon><t-icon name="refresh" /></template>
            刷新
          </t-button>
        </t-space>
        <span class="toolbar__count">{{ countText }}</span>
      </div>
      <div v-if="data" class="toolbar__meta">
        窗口 {{ data.range.start }} ~ {{ data.range.end }}（{{ data.range.days }} 天） · 任务
        {{ data.tasks.length }} 条 · 依赖 {{ data.dependencies.length }} 条
        <span class="toolbar__tag">可拖拽改期</span>
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
      <!-- 图表控制条：紧贴画布（浏览时高频操作，视线不用离开图表） -->
      <div class="gantt-ctrl">
        <t-space align="center">
          <t-button size="small" variant="outline" @click="shiftWindow(-1)">
            <template #icon><t-icon name="chevron-left" /></template>
            上一屏
          </t-button>
          <t-button size="small" variant="outline" data-test="gantt-today" @click="goToday">今天</t-button>
          <t-button size="small" variant="outline" @click="shiftWindow(1)">
            下一屏
            <template #suffix><t-icon name="chevron-right" /></template>
          </t-button>
          <t-divider layout="vertical" />
          <t-tabs v-model="zoom" class="mini-tabs" :space-evenly="false">
            <t-tab-panel value="day" label="日" />
            <t-tab-panel value="week" label="周" />
          </t-tabs>
          <t-tabs v-model="scope" class="mini-tabs" :space-evenly="false">
            <t-tab-panel value="all" label="全部可见" />
            <t-tab-panel value="my" label="只看我的" />
          </t-tabs>
        </t-space>
      </div>
      <t-loading :loading="loading" size="small">
        <!-- 空态：没有任何可排期任务（都没有 start_date / due_date） -->
        <div v-if="!loading && visibleTasks.length === 0" class="gantt-empty">
          <t-icon name="calendar" size="32px" class="gantt-empty__icon" />
          <p class="gantt-empty__text">
            这个时间窗内没有可排期的任务（任务需要有「计划开始」或「截止日期」才能画到甘特图上）
          </p>
          <t-button size="small" variant="outline" @click="goToday">回到今天</t-button>
        </div>

        <!-- 甘特主体：**纯 SVG 手绘**（不引任何甘特库；拖拽用原生 pointer 事件） -->
        <div v-else ref="scrollRef" class="gantt-scroll">
          <svg
            class="gantt-svg"
            :width="svgWidth"
            :height="svgHeight"
            :viewBox="`0 0 ${svgWidth} ${svgHeight}`"
            role="img"
            aria-label="任务甘特图（可拖拽改期）"
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
              <!-- 折叠箭头（只有父任务才有孩子）：点它只控制子任务条形显隐 -->
              <text
                v-if="row.childCount > 0"
                :x="8 + row.depth * INDENT"
                :y="rowTop(index) + ROW_H / 2 + 4"
                class="gantt-toggle"
                :data-toggle="row.task.task_no"
                @click="toggle(row.id)"
              >
                {{ collapsed.has(row.id) ? '▸' : '▾' }}
              </text>
              <!-- 子任务的树枝（├ / └）：一眼看出层级，不靠纯缩进猜 -->
              <text
                v-if="row.depth > 0"
                :x="12"
                :y="rowTop(index) + ROW_H / 2 + 4"
                class="gantt-branch"
              >
                {{ isLastChild(index) ? '└' : '├' }}
              </text>
              <circle
                :cx="24 + row.depth * INDENT"
                :cy="rowTop(index) + ROW_H / 2"
                r="3.5"
                :fill="statusColor(row.task.status)"
              />
              <text
                :x="34 + row.depth * INDENT"
                :y="rowTop(index) + ROW_H / 2 + 4"
                class="gantt-name"
                :class="{ 'gantt-name--done': row.task.status === 'DONE', 'gantt-name--child': row.depth > 0 }"
                :data-depth="row.depth"
                :data-task-no="row.task.task_no"
              >
                {{ clip(row.task.title, 16 - row.depth * 2) }}
              </text>
            </g>

            <!-- ③ 任务条：父=深（brand-7）/子=浅（brand-5）、内条按进度、逾期描红、被阻塞加斜纹、可拖拽改期 -->
            <g v-for="(row, index) in rows" :key="`bar-${row.id}`">
              <template v-if="barGeom(row)">
                <rect
                  :x="barGeom(row)!.x"
                  :y="rowTop(index) + 8"
                  :width="barGeom(row)!.width"
                  :height="BAR_H"
                  rx="4"
                  :fill="barColor(row)"
                  :fill-opacity="row.depth > 0 ? 0.16 : 0.3"
                  :stroke="row.task.overdue ? cssVar('--td-error-color', '#d54941') : barColor(row)"
                  :stroke-width="row.task.overdue ? 2 : 1"
                  :stroke-opacity="row.task.overdue ? 1 : row.depth > 0 ? 0.6 : 1"
                  class="gantt-bar gantt-bar__body"
                  :class="[
                    canDrag(row.task) ? 'gantt-bar__body--draggable' : 'gantt-bar__body--locked',
                    row.depth > 0 ? 'gantt-bar__body--child' : 'gantt-bar__body--parent',
                    dragState?.id === row.id ? 'gantt-bar--dragging' : '',
                  ]"
                  :style="barTransition(row)"
                  :data-task-no="row.task.task_no"
                  :data-depth="row.depth"
                  :data-bar-kind="row.depth > 0 ? 'child' : 'parent'"
                  :data-parent-id="row.task.parent_id ?? ''"
                  :data-summary="isSummary(row.task) ? 'true' : 'false'"
                  @pointerdown="onPointerDown($event, row, 'move')"
                  @click="onBarClick(row.task)"
                >
                  <title>{{ tooltipOf(row.task) }}</title>
                </rect>
                <!-- 被阻塞：斜纹覆盖（只覆盖未完成的部分，代表「还不能开始」） -->
                <rect
                  v-if="row.task.blocked"
                  :x="barGeom(row)!.x"
                  :y="rowTop(index) + 8"
                  :width="barGeom(row)!.width"
                  :height="BAR_H"
                  rx="4"
                  fill="url(#gantt-blocked)"
                  fill-opacity="0.55"
                  class="gantt-bar gantt-bar__overlay"
                  :style="barTransition(row)"
                  @click="onBarClick(row.task)"
                />
                <!-- 进度内条（progress% 宽度）：与条形同色系，0.85 不透明避免过淡看不清 -->
                <rect
                  v-if="row.task.progress > 0"
                  :x="barGeom(row)!.x"
                  :y="rowTop(index) + 8 + BAR_H - 5"
                  :width="Math.max(2, (barGeom(row)!.width * row.task.progress) / 100)"
                  :height="5"
                  rx="2"
                  :fill="barColor(row)"
                  :fill-opacity="row.depth > 0 ? 0.7 : 0.95"
                  class="gantt-bar gantt-bar__overlay"
                  :style="barTransition(row)"
                  @click="onBarClick(row.task)"
                />
                <!-- 汇总括号：父任务的条形是子任务汇总时，两端加一小段竖线（一眼区分「自身排期」与「汇总」） -->
                <template v-if="isSummary(row.task)">
                  <line
                    :x1="barGeom(row)!.x + 1"
                    :y1="rowTop(index) + 6"
                    :x2="barGeom(row)!.x + 1"
                    :y2="rowTop(index) + 8 + BAR_H + 2"
                    :stroke="barColor(row)"
                    stroke-width="2"
                    class="gantt-bar__overlay"
                  />
                  <line
                    :x1="barGeom(row)!.x + barGeom(row)!.width - 1"
                    :y1="rowTop(index) + 6"
                    :x2="barGeom(row)!.x + barGeom(row)!.width - 1"
                    :y2="rowTop(index) + 8 + BAR_H + 2"
                    :stroke="barColor(row)"
                    stroke-width="2"
                    class="gantt-bar__overlay"
                  />
                </template>
                <!-- 左右边缘把手：只有可拖拽的任务才渲染（不可拖拽的任务不出现假入口） -->
                <template v-if="canDrag(row.task)">
                  <rect
                    :x="barGeom(row)!.x - 3"
                    :y="rowTop(index) + 4"
                    :width="8"
                    :height="BAR_H + 8"
                    fill="transparent"
                    class="gantt-bar__handle"
                    :data-handle="`${row.task.task_no}-start`"
                    @pointerdown="onPointerDown($event, row, 'start')"
                  />
                  <rect
                    :x="barGeom(row)!.x + barGeom(row)!.width - 5"
                    :y="rowTop(index) + 4"
                    :width="8"
                    :height="BAR_H + 8"
                    fill="transparent"
                    class="gantt-bar__handle"
                    :data-handle="`${row.task.task_no}-end`"
                    @pointerdown="onPointerDown($event, row, 'end')"
                  />
                </template>
              </template>
            </g>

            <!-- ④ 依赖线：前置结束 → 后继开始（折线 + 箭头）；拖动时随条形实时移动 -->
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
                class="gantt-link"
                :data-link="`${link.from}->${link.to}`"
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

          <!-- 拖拽中的浮动日期标签（实时跟随指针；松手后消失） -->
          <div
            v-if="dragState && dragState.moved"
            class="gantt-drag-label"
            data-test="gantt-drag-label"
            :style="{ left: `${dragState.labelX}px`, top: `${dragState.labelY}px` }"
          >
            {{ dragLabel }}
          </div>
        </div>
      </t-loading>
    </t-card>

    <!-- 点任务条 → T6a 的详情抽屉（同一份组件，不重复实现） -->
    <TaskDetail v-model:visible="detailVisible" :task-id="detailTaskId" @deleted="load" />
    <TaskForm v-model:visible="formVisible" @saved="handleSaved" />
  </div>
</template>

<script setup lang="ts">
/**
 * 甘特图（批次 C+ · T4 只读 → **T7a 拖拽改期** → **E6c 父子折叠 + 汇总条**）。
 *
 * 依据：`docs/批次C+-工作任务管理-核心设计.md` §五（甘特图）、
 * `docs/云枢设计修正补丁-20261029.md` **修正 10**（拖拽改期必须重跑依赖校验）、
 * 卡面 t_52ce6d07「二、前端」、卡面 t_2cfbd517「二、甘特图（E6c）」。
 *
 * # E6c 父子（本页新增）
 *
 * - 层级：父任务 0 缩进、子任务 +16px（`INDENT`），子行前带 `├`/`└` 树枝；
 * - 折叠：父行前的 `▾`/`▸` 只控制**子任务条形显隐**，折叠后父条仍显示（汇总值）；
 * - 父条 = 子任务汇总：`min(子 start)` ~ `max(子 due)`，与父自身日期取**并集**
 *   （父条必须包住孩子；父无自身日期时退化纯子任务汇总，见 `summarySpan`）；
 * - 颜色：父 `--td-brand-color-7`（深）、子 `--td-brand-color-5`（浅），
 *   状态仍由行首圆点 + tooltip 表达（不丢信息）；
 * - 父子之间不画依赖线（后端 `63103` 也拒绝父子互依赖，前端再按 `parent_id` 兜一层）。
 *
 * # 五种拖法 ↔ 三种请求形态
 *
 * | 拖哪里 | 改什么 | 请求 |
 * |--------|--------|------|
 * | 条形中部 | 起止日一起平移 | `{ shift_days: N }` |
 * | 左边缘 | 只改开始日 | `{ start_date: 'YYYY-MM-DD' }` |
 * | 右边缘 | 只改截止日 | `{ due_date: 'YYYY-MM-DD' }` |
 *
 * # 四条交互纪律（卡面红线）
 *
 * 1. **乐观更新 + 失败回弹**：松手立刻把条形移到新位置（并写入数据模型），再发请求；
 *    失败时把快照写回去 —— 条形带 `transition: x/width .28s` 平滑回弹，**绝不留下假状态**。
 * 2. **失败提示点名冲突对象**：文案直接用后端 `msg`（`63110`/`63111`/`63112` 已含
 *    `task_no + 标题 + 日期`）。前端不自己拼一套 —— 拼两套迟早不一致。
 * 3. **不可拖拽的任务明确禁用**：`DONE` / `CANCELLED` → `cursor: not-allowed` +
 *    点一下给出原因；只有单边日期（缺开始或截止）→ 同样禁用并提示「请先设置起止日期」。
 *    **不静默失败**（拖了没反应是最糟的交互）。
 * 4. **依赖线实时跟随**：`links` 全部由 `barGeom()` 推导，拖动中几何量变了线自然跟着走；
 *    没有第二套「拖拽专用」坐标。
 *
 * # 几何量：一套换算，两种输入
 *
 * 像素换算**只有** [`geomOfSpan`] 一个入口（「日期 × 列宽」，含裁到时间窗内，
 * 与后端 `dep::bar_span` 同口径）。滚动条的输入有两种：
 * ① 叶子任务 = 自身 `start_date / due_date`；② 父任务 = `summarySpan` 的并集区间。
 * 拖动预演、松手写库、回弹都用同一套换算，避免「拖动时一种算法、落库后又一种算法」。
 * （E6c 起不再直接读 `task.bar.offset_days`：父任务的汇总区间后端不提供，
 *  为了让父子两类条形走同一条路径，统一由日期算。）
 */
import { computed, nextTick, onMounted, ref, watch } from 'vue';
import { useRouter } from 'vue-router';
import dayjs from 'dayjs';
import { MessagePlugin } from 'tdesign-vue-next';
import TaskDetail from './detail.vue';
import UserSelect from './components/UserSelect.vue';
import TaskForm from './components/TaskForm.vue';
import { ApiError } from '@/api/request';
import { pageProjects } from '@/api/business/project-api';
import {
  fetchGantt,
  scheduleTask,
  type GanttTask,
  type TaskGantt,
  type TaskScheduleReq,
} from '@/api/business/task-api';
import { statusMeta } from '@/constants/status-map';
import { PERM } from '@/constants/permissions';

/** 布局常量（像素）：左侧任务名宽度、行高、表头高度、任务条高度 */
const LEFT_W = 280;
const ROW_H = 34;
const HEAD_H = 46;
const BAR_H = 18;
/** 子任务相对父任务的缩进（E6c：父 0、子 +16px） */
const INDENT = 16;

/** 缩放：每天多少像素（日 / 周两档） */
const DAY_WIDTH: Record<'day' | 'week', number> = { day: 30, week: 12 };

/** 日期区间（`YYYY-MM-DD`，含首含尾；只用于几何换算与汇总，不再回写后端） */
interface DateSpan {
  start: string;
  due: string;
}

/** 拖拽形态：整体平移 / 改开始日 / 改截止日 */
type DragMode = 'move' | 'start' | 'end';

/** 拖拽中的状态（只在指针按下期间存在；松手后几何量已经写进数据模型） */
interface DragState {
  /** 任务 id */
  id: string;
  /** 任务编号（浮动标签用） */
  taskNo: string;
  /** 拖拽形态 */
  mode: DragMode;
  /** 按下时的指针 x（视口坐标） */
  startClientX: number;
  /** 按下时的指针 y（视口坐标） */
  startClientY: number;
  /** 当前位移（天，四舍五入到整日） */
  deltaDays: number;
  /** 是否真的移动过（没动过 = 当点击处理，不发请求） */
  moved: boolean;
  /** 浮动标签相对滚动容器的坐标 */
  labelX: number;
  labelY: number;
  /** 原始日期（回弹用） */
  origStart: string;
  origDue: string;
  /** 原始条形几何（回弹用；后端给的天数） */
  origBar: { offset_days: number; length_days: number } | null;
}

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
/** 负责人筛选（GanttQuery.assignee_id；空 = 不筛） */
const filterAssignee = ref<string | null>(null);
/** 项目筛选（GanttQuery.project_id；空 = 不筛） */
const filterProject = ref<string | undefined>(undefined);
/** 项目下拉选项 */
const projectOptions = ref<Array<{ label: string; value: string }>>([]);
/** 项目下拉加载中 */
const projectLoading = ref(false);
/** 新建任务弹窗 */
const formVisible = ref(false);

/** 工具条计数文案（与 index.vue 同款） */
const countText = computed(() => {
  const scopeText = scope.value === 'my' ? '只看我的' : '全部可见任务';
  if (!data.value) return `${scopeText} · 加载中`;
  return `${scopeText} · 共 ${data.value.tasks.length} 条`;
});

/** 新建任务 */
function openCreate(): void {
  formVisible.value = true;
}

/** 新建成功后重拉甘特（新任务可能落在当前时间窗内） */
function handleSaved(): void {
  void load();
}

/** 拉项目下拉（筛选用；失败不影响甘特图） */
async function loadProjectOptions(): Promise<void> {
  projectLoading.value = true;
  try {
    const data = await pageProjects({ page: 1, page_size: 200 });
    projectOptions.value = data.list.map((item) => ({
      label: `${item.code} ${item.name}`,
      value: item.id,
    }));
  } catch {
    projectOptions.value = [];
  } finally {
    projectLoading.value = false;
  }
}

/** 点「查询」：用当前筛选条件重新拉数据（不 watch 输入，避免每敲一个字符就查） */
function handleFilter(): void {
  void load();
}

/** 点「重置」：清空筛选 + 重查 */
function handleFilterReset(): void {
  filterAssignee.value = null;
  filterProject.value = undefined;
  void load();
}
/** 显式时间窗（清空 → 后端默认今天起 30 天；与列表页的 date-range 同样用 string[]） */
const windowRange = ref<string[]>([]);
/** 折叠的父任务 id */
const collapsed = ref<Set<string>>(new Set());
/** 详情抽屉 */
const detailVisible = ref(false);
/** 详情抽屉的任务 id */
const detailTaskId = ref<string | null>(null);
/** 拖拽中的状态 */
const dragState = ref<DragState | null>(null);
/** 滚动的容器（「今天」按钮要滚到当前日期；浮动标签也定位在它里面） */
const scrollRef = ref<HTMLElement | null>(null);
/** 正在保存（防同一任务并发两次请求） */
const saving = ref<Set<string>>(new Set());

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

/**
 * ==================== E6c：父子汇总（父条 = 子任务汇总） ====================
 *
 * 口径（照卡面 E6c §二.3）：
 * - 起点 = `min(所有子任务 start_date)`、终点 = `max(所有子任务 due_date)`；
 * - 父任务自己**没有**日期但有子任务时 → 直接用子任务汇总。
 *
 * 实现取**并集**：`min(父自身 start, 子任务 min)` ~ `max(父自身 due, 子任务 max)`。
 * 为什么不是纯子任务 min/max：父条必须能**包住**自己的孩子（否则子条伸出父条，
 * 看着像数据错了）；当父自身日期落在子任务范围内时，并集 == 子任务汇总，
 * 与卡面口径逐字一致；父任务无自身日期时并集退化成子任务汇总（卡面第二条边界）。
 *
 * 已知边界（后端口径决定，不是前端能补的）：`gantt_tasks` 的 SQL 带
 * `(t.start_date IS NOT NULL OR t.due_date IS NOT NULL)`，**完全无日期的父任务不下发**，
 * 那时其子任务会以「根节点」出现。后端一旦把无日期父任务也返回，本逻辑无需改动。
 */

/** 任务自身日期 → 区间（单边日期按单日处理；无日期 → null） */
function spanOfDates(start: string | null, due: string | null): DateSpan | null {
  const from = start ?? due;
  const to = due ?? start;
  if (!from || !to) {
    return null;
  }
  return to < from ? { start: from, due: from } : { start: from, due: to };
}

/** parent_id → 子任务汇总区间（一次遍历，O(n)，不按行重复算） */
const childSpans = computed<Map<string, DateSpan>>(() => {
  const out = new Map<string, DateSpan>();
  for (const task of data.value?.tasks ?? []) {
    if (!task.parent_id) {
      continue;
    }
    const span = spanOfDates(task.start_date, task.due_date);
    if (!span) {
      continue;
    }
    const current = out.get(task.parent_id);
    if (!current) {
      out.set(task.parent_id, { ...span });
      continue;
    }
    out.set(task.parent_id, {
      start: span.start < current.start ? span.start : current.start,
      due: span.due > current.due ? span.due : current.due,
    });
  }
  return out;
});

/** 父任务自身日期 ∪ 子任务汇总（无子任务 → 自身日期；都无 → null） */
function summarySpan(task: GanttTask): DateSpan | null {
  const own = spanOfDates(task.start_date, task.due_date);
  const kids = childSpans.value.get(task.id);
  if (!kids) {
    return own;
  }
  if (!own) {
    return kids;
  }
  return {
    start: own.start < kids.start ? own.start : kids.start,
    due: own.due > kids.due ? own.due : kids.due,
  };
}

/** 该行是否是「汇总条」（有子任务参与汇总）—— 两端会画汇总括号 */
function isSummary(task: GanttTask): boolean {
  return childSpans.value.has(task.id);
}

/** 子任务树枝符号用：本行是否是父任务折叠块的最后一行 */
function isLastChild(index: number): boolean {
  const next = rows.value[index + 1];
  return !next || next.depth === 0;
}

/** 条形颜色：父 `--td-brand-color-7`（深）/ 子 `--td-brand-color-5`（浅） */
function barColor(row: GanttRow): string {
  return row.depth > 0
    ? cssVar('--td-brand-color-5', '#7ba7f0')
    : cssVar('--td-brand-color-7', '#0052d9');
}

/** 「日期区间 + 窗口起点 + 列宽」→ 像素几何（唯一换算入口） */
function geomOfSpan(
  start: string,
  due: string,
): { x: number; width: number; offsetDays: number; lengthDays: number } | null {
  const range = data.value?.range;
  if (!range) {
    return null;
  }
  const rawOffset = dayjs(start).startOf('day').diff(dayjs(range.start).startOf('day'), 'day');
  const rawLength = Math.max(1, dayjs(due).startOf('day').diff(dayjs(start).startOf('day'), 'day') + 1);
  // 与后端 `dep::bar_span` 同口径：**裁到时间窗内**。
  // 不裁的话，拖动一个「窗口左边界之前开始」的任务时，条形会画到 x < LEFT_W 的
  // 任务名区域里（实测 x=160 < 280），松手重新拉数据后才被后端裁回来 —— 视觉上「跳一下」。
  const maxDays = Math.max(1, range.days);
  const clippedStart = Math.max(0, Math.min(maxDays - 1, rawOffset));
  const clippedEnd = Math.min(maxDays, rawOffset + rawLength);
  const offsetDays = clippedStart;
  const lengthDays = Math.max(1, clippedEnd - clippedStart);
  return {
    x: LEFT_W + offsetDays * dayWidth.value,
    width: Math.max(6, lengthDays * dayWidth.value - 2),
    offsetDays,
    lengthDays,
  };
}

/** 拖动中的预演区间（未拖动 → null） */
const previewSpan = computed<{ start: string; due: string } | null>(() => {
  const state = dragState.value;
  if (!state || !state.moved) {
    return null;
  }
  return computeSpan(state.origStart, state.origDue, state.mode, state.deltaDays);
});

/**
 * 拖拽区间换算（**唯一实现**）：拖拽中预览与松手后的请求体都走这里，
 * 避免「拖动时一种算法、落库后又一种算法」导致条形跳一下。
 *
 * 边界：左边缘不许越过右边缘、右边缘不许越过左边缘（顺序倒置在前端先自证，
 * 少一次 `400 + 63004`；真正的合法性仍以后端为准）。
 */
function computeSpan(
  origStart: string,
  origDue: string,
  mode: DragMode,
  deltaDays: number,
): { start: string; due: string } {
  let start = dayjs(origStart).startOf('day');
  let due = dayjs(origDue).startOf('day');
  if (mode === 'move') {
    start = start.add(deltaDays, 'day');
    due = due.add(deltaDays, 'day');
  } else if (mode === 'start') {
    start = start.add(deltaDays, 'day');
    if (start.isAfter(due)) {
      start = due;
    }
  } else {
    due = due.add(deltaDays, 'day');
    if (due.isBefore(start)) {
      due = start;
    }
  }
  return { start: start.format('YYYY-MM-DD'), due: due.format('YYYY-MM-DD') };
}

/** 任务条几何：拖动中的那一行用预演值，其余用「自身 ∪ 子任务汇总」的区间 */
function barGeom(row: GanttRow): { x: number; width: number } | null {
  const preview = previewSpan.value;
  if (preview && dragState.value?.id === row.id) {
    const geom = geomOfSpan(preview.start, preview.due);
    return geom ? { x: geom.x, width: geom.width } : null;
  }
  const span = summarySpan(row.task);
  if (!span) {
    return null;
  }
  // 与后端口径一致：几何量一律经 `geomOfSpan`（含裁到时间窗内），
  // 父任务的并集区间同样走这里 —— 不另起一套「父条算法」。
  const geom = geomOfSpan(span.start, span.due);
  return geom ? { x: geom.x, width: geom.width } : null;
}

/** 浮动标签文案：`2026-11-04 ~ 2026-11-08（+3 天）` */
const dragLabel = computed(() => {
  const state = dragState.value;
  const span = previewSpan.value;
  if (!state || !span) {
    return '';
  }
  const delta = state.deltaDays >= 0 ? `+${state.deltaDays}` : `${state.deltaDays}`;
  const label = state.mode === 'move' ? '整体平移' : state.mode === 'start' ? '改开始日' : '改截止日';
  return `${label}：${span.start} ~ ${span.due}（${delta} 天）`;
});

/** 条形 transition（拖动中的那一行关掉，避免「跟手迟滞」；其余时刻保留以便回弹） */
function barTransition(row: GanttRow): string {
  // 注意：`:style` 传字符串时必须是**合法声明**（含属性名），
  // 只写 `x 0.28s ease` 会被浏览器当非法声明丢掉（实测：style 属性为空、回弹瞬移）。
  return dragState.value?.id === row.id
    ? 'transition: none'
    : 'transition: x 0.28s ease, width 0.28s ease';
}

/**
 * 依赖线：前置条右端 → 后继条左端（折线，拐点在中缝）；拖动时随 barGeom 跟随。
 *
 * **父子之间不画依赖线**（E6c §二.5）：父子是拆解关系，不是依赖关系。
 * 后端 `biz_task_dependency` 里理论上可以存出一条「父 → 子」的边（用户手加的），
 * 这里按 `parent_id` 关系直接跳过，避免树形上出现一条自上而下的假箭头。
 */
const links = computed(() => {
  const out: { from: string; to: string; points: string }[] = [];
  const parentOf = new Map<string, string>();
  for (const task of data.value?.tasks ?? []) {
    if (task.parent_id) {
      parentOf.set(task.id, task.parent_id);
    }
  }
  for (const dep of data.value?.dependencies ?? []) {
    // 父子关系不是依赖：任一方向都跳过
    if (parentOf.get(dep.from) === dep.to || parentOf.get(dep.to) === dep.from) {
      continue;
    }
    const predIndex = rowIndexById.value.get(dep.from);
    const succIndex = rowIndexById.value.get(dep.to);
    if (predIndex === undefined || succIndex === undefined) {
      continue; // 一端被折叠 / 不在窗口内 → 不画
    }
    const pred = barGeom(rows.value[predIndex] ?? { id: '', task: {} as GanttTask, depth: 0, childCount: 0 });
    const succ = barGeom(rows.value[succIndex] ?? { id: '', task: {} as GanttTask, depth: 0, childCount: 0 });
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

/** 条形 tooltip（标题 / 编号 / 日期 / 进度 / 负责人 / 状态；父任务额外说明汇总口径） */
function tooltipOf(task: GanttTask): string {
  const span = summarySpan(task);
  const own = spanOfDates(task.start_date, task.due_date);
  const range =
    span && isSummary(task)
      ? `汇总 ${span.start} ~ ${span.due}（自身 ${task.start_date ?? '—'} ~ ${task.due_date ?? '—'}）`
      : `${own?.start ?? '—'} ~ ${own?.due ?? '—'}`;
  const flags = [
    task.overdue ? '已逾期' : '',
    task.blocked ? '被前置阻塞' : '',
    statusMeta('task', task.status).label,
    isSummary(task) ? '父任务（条形 = 子任务汇总）' : '',
    canDrag(task) ? '可拖拽改期' : '不可拖拽',
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

/** 打开详情抽屉（点任务条唯一的交互；拖动过的那一下不打开） */
function openDetail(id: string): void {
  detailTaskId.value = id;
  detailVisible.value = true;
}

// ==================== T7a：拖拽改期 ====================

/** 是否可拖拽改期：终态不可拖；起止日必须都有（只有单边日期的任务拖出来的语义含糊） */
function canDrag(task: GanttTask): boolean {
  if (task.status === 'DONE' || task.status === 'CANCELLED') {
    return false;
  }
  return Boolean(task.start_date && task.due_date);
}

/** 不可拖拽的原因（**必须说出来**，不许静默失败） */
function disabledReason(task: GanttTask): string {
  if (task.status === 'DONE') {
    return '已完成的任务不可拖拽改期';
  }
  if (task.status === 'CANCELLED') {
    return '已取消的任务不可拖拽改期';
  }
  const kids = childSpans.value.get(task.id);
  if (kids && !(task.start_date && task.due_date)) {
    // E6c 边界：父任务自身没有完整起止日 → 条形是子任务汇总，拖它改不了任何东西
    return `父任务自身没有完整的起止日期（条形是子任务汇总 ${kids.start} ~ ${kids.due}），请在子任务上改期`;
  }
  return '请先设置起止日期（任务需要「计划开始」和「截止日期」都有值才能拖拽改期）';
}

/** 点击任务条：拖动过就当成拖拽，不打开详情 */
let suppressClick = false;
function onBarClick(task: GanttTask): void {
  if (suppressClick) {
    suppressClick = false;
    return;
  }
  openDetail(task.id);
}

/** 指针按下：起拖（中部 / 左边缘 / 右边缘） */
function onPointerDown(event: PointerEvent, row: GanttRow, mode: DragMode): void {
  if (event.button !== 0) {
    return;
  }
  const task = row.task;
  if (!canDrag(task)) {
    event.preventDefault();
    MessagePlugin.warning(disabledReason(task));
    return;
  }
  if (saving.value.has(task.id)) {
    MessagePlugin.warning('上一次改期还在保存，请稍候');
    return;
  }
  event.preventDefault();
  event.stopPropagation();
  dragState.value = {
    id: task.id,
    taskNo: task.task_no,
    mode,
    startClientX: event.clientX,
    startClientY: event.clientY,
    deltaDays: 0,
    moved: false,
    labelX: labelCoords(event).x,
    labelY: labelCoords(event).y,
    origStart: task.start_date as string,
    origDue: task.due_date as string,
    origBar: task.bar ? { ...task.bar } : null,
  };
  window.addEventListener('pointermove', onPointerMove);
  window.addEventListener('pointerup', onPointerUp);
  window.addEventListener('pointercancel', onPointerUp);
}

/** 浮动标签相对滚动容器的坐标（容器可能已横向 / 纵向滚动） */
function labelCoords(event: PointerEvent): { x: number; y: number } {
  const container = scrollRef.value;
  if (!container) {
    return { x: event.clientX, y: event.clientY };
  }
  const rect = container.getBoundingClientRect();
  return {
    x: event.clientX - rect.left + container.scrollLeft + 12,
    y: event.clientY - rect.top + container.scrollTop - 28,
  };
}

/** 指针移动：实时换算天数 + 移动浮动标签（条形几何由 `previewSpan` 推导） */
function onPointerMove(event: PointerEvent): void {
  const state = dragState.value;
  if (!state) {
    return;
  }
  const rawDays = (event.clientX - state.startClientX) / dayWidth.value;
  const deltaDays = Math.round(rawDays);
  const coords = labelCoords(event);
  // 纵向拖动不参与计算（本卡只做水平改期），但标签要跟着手走
  dragState.value = {
    ...state,
    deltaDays,
    moved: deltaDays !== 0,
    labelX: coords.x,
    labelY: coords.y,
  };
}

/** 指针松开：乐观更新 → 发请求 → 失败回弹 */
function onPointerUp(): void {
  window.removeEventListener('pointermove', onPointerMove);
  window.removeEventListener('pointerup', onPointerUp);
  window.removeEventListener('pointercancel', onPointerUp);
  const state = dragState.value;
  dragState.value = null;
  if (!state) {
    return;
  }
  suppressClick = state.moved;
  if (!state.moved) {
    return; // 没拖动 = 一次点击（详情抽屉照常打开）
  }
  const preview = spanOfState(state);
  const task = data.value?.tasks.find((item) => item.id === state.id);
  if (!task || !preview) {
    return;
  }
  // ① 乐观更新：先把条形挪过去（几何量与拖动中完全一致 → 不会跳）
  const geom = geomOfSpan(preview.start, preview.due);
  task.start_date = preview.start;
  task.due_date = preview.due;
  if (geom) {
    task.bar = { offset_days: geom.offsetDays, length_days: geom.lengthDays };
  }
  void commitSchedule(state, preview, task);
}

/** 某次拖拽对应的目标区间 */
function spanOfState(state: DragState): { start: string; due: string } | null {
  return computeSpan(state.origStart, state.origDue, state.mode, state.deltaDays);
}

/** 请求体：三种拖法 ↔ 三种形态（二选一，后端会拒绝同时传） */
function buildPayload(state: DragState, preview: { start: string; due: string }): TaskScheduleReq {
  if (state.mode === 'move') {
    return { shift_days: state.deltaDays };
  }
  if (state.mode === 'start') {
    return { start_date: preview.start };
  }
  return { due_date: preview.due };
}

/** 提交改期：成功用贴库值校正，失败**必须回弹**并点名冲突对象 */
async function commitSchedule(
  state: DragState,
  preview: { start: string; due: string },
  task: GanttTask,
): Promise<void> {
  const payload = buildPayload(state, preview);
  saving.value = new Set(saving.value).add(state.id);
  try {
    const result = await scheduleTask(state.id, payload);
    // 贴库值校正（后端可能因为依赖 / 父子校验换算出别的日期？不会，但只信回执）
    const span = { start: result.start_date as string, due: result.due_date as string };
    const geom = geomOfSpan(span.start, span.due);
    task.start_date = span.start;
    task.due_date = span.due;
    if (geom) {
      task.bar = { offset_days: geom.offsetDays, length_days: geom.lengthDays };
    }
    MessagePlugin.success(`已改期：${result.old_start_date ?? '—'} ~ ${result.old_due_date ?? '—'} → ${span.start} ~ ${span.due}`);
    // 依赖线 / 阻塞标记 / 逾期标记都依赖新日期，重新拉一次保证与库一致
    await load();
  } catch (error) {
    await rollback(state);
    MessagePlugin.error(
      error instanceof ApiError
        ? error.message
        : error instanceof Error
          ? error.message
          : '改期失败，请稍后重试',
    );
  } finally {
    const next = new Set(saving.value);
    next.delete(state.id);
    saving.value = next;
  }
}

/** 回弹到拖拽前的快照（条形带 transition，视觉上是平滑退回） */
async function rollback(state: DragState): Promise<void> {
  const task = data.value?.tasks.find((item) => item.id === state.id);
  if (!task) {
    return;
  }
  const rect = scrollRef.value?.querySelector(
    `rect.gantt-bar__body[data-task-no="${state.taskNo}"]`,
  );
  // 关键：先「强制一次样式计算」，确认 transition 已经在生效状态，再写回旧值。
  // 否则「切回 transition」与「改回 x」落在同一帧 → 浏览器认为前后样式没有过渡声明，
  // 条形会瞬移回原位（实测就是这样，不是 CSS 不支持：SVG 几何属性 x/width 从
  // Chrome 84 起就是可过渡的 CSS 属性，本机 HeadlessChrome 145 实测 460→326 有插值）。
  forceStyle(rect);
  await nextTick();
  forceStyle(rect);
  task.start_date = state.origStart;
  task.due_date = state.origDue;
  task.bar = state.origBar ? { ...state.origBar } : task.bar;
}

/** 读一次 SVG 几何属性，迫使浏览器结算样式（等价于读 `offsetWidth` 的老办法） */
function forceStyle(element: Element | null | undefined): void {
  if (element) {
    void getComputedStyle(element).x;
  }
}

/**
 * 回列表页 / 看板页（三视图切换）
 */
function handleViewChange(value: unknown): void {
  const next = String(value);
  if (next === 'board') {
    void router.push('/task?view=board');
    return;
  }
  if (next === 'burndown') {
    void router.push('/task?view=burndown');
    return;
  }
  if (next === 'list') {
    void router.push('/task');
  }
  // `gantt` = 当前页，不动作
}

/** 时间窗变更是否要静默（「今天」按钮自己会 load，避免两次请求） */
let quietWindowChange = false;

/** 「今天」：把今天滚到视口中间（今天不在当前窗口 → 先回到默认窗口） */
async function goToday(): Promise<void> {
  const range = data.value?.range;
  const inWindow = Boolean(range) && range?.today_offset !== null && range?.today_offset !== undefined;
  if (!inWindow) {
    quietWindowChange = true;
    windowRange.value = [];
    await nextTick();
    quietWindowChange = false;
    await load();
  }
  await nextTick();
  scrollToToday();
}

/** 滚动到今天的竖线（无今天线 → 不动作） */
function scrollToToday(): void {
  const container = scrollRef.value;
  const x = todayX.value;
  if (!container || x === null) {
    return;
  }
  const target = x - container.clientWidth / 2;
  container.scrollTo({ left: Math.max(0, target), behavior: 'smooth' });
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
      assignee_id: filterAssignee.value || undefined,
      project_id: filterProject.value || undefined,
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
  if (quietWindowChange) {
    return;
  }
  void load();
});
// 缩放变化时保持折叠状态，仅重算像素（computed 自动跟随，这里只留一处显式说明）
watch(zoom, () => {
  /* 无需请求：几何量全部由 computed 推导 */
});

onMounted(() => {
  void loadProjectOptions();
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

.toolbar-card {
  :deep(.t-card__body) {
    padding: 10px 16px;
  }
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

.toolbar__hint {
  margin-top: 4px;
  font-size: 12px;
  color: var(--td-text-color-placeholder, #999);

  b {
    color: var(--td-text-color-secondary, #666);
    font-weight: 600;
  }
}

.toolbar__warn {
  color: var(--td-warning-color, #e37318);
}

.toolbar__hint-extra {
  color: var(--td-text-color-placeholder, #999);
}

.gantt-scroll {
  position: relative;
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

  &:hover {
    fill: var(--td-brand-color, #0052d9);
  }
}

// 子任务树枝（├ / └）：与父行缩进一起构成「树」的感觉
.gantt-branch {
  font-size: 11px;
  fill: var(--td-component-stroke, #dcdcdc);
  pointer-events: none;
}

// 子任务名比父任务名浅一档（层级靠缩进 + 字色一起表达）
.gantt-name--child {
  fill: var(--td-text-color-secondary, #666);
}

.gantt-bar {
  cursor: pointer;
}

// 可拖：中部 `grab` / 抓住时 `grabbing`；不可拖：`not-allowed`（**明确禁用**，不静默失败）
.gantt-bar__body--draggable {
  cursor: grab;

  &:active {
    cursor: grabbing;
  }
}

.gantt-bar__body--locked {
  cursor: not-allowed;
}

.gantt-bar--dragging {
  opacity: 0.86;
}

// 斜纹覆盖层与进度内条不该吃掉指针事件（否则按不到条形本体与边缘把手）
.gantt-bar__overlay {
  pointer-events: none;
}

.gantt-bar__handle {
  cursor: ew-resize;
}

.gantt-link {
  pointer-events: none;
}

.gantt-drag-label {
  position: absolute;
  z-index: 10;
  padding: 2px 8px;
  border-radius: 4px;
  font-size: 12px;
  line-height: 20px;
  white-space: nowrap;
  color: var(--td-text-color-anti, #ffffff);
  background: var(--td-brand-color, #0052d9);
  box-shadow: 0 2px 8px rgb(0 0 0 / 16%);
  pointer-events: none;
}

.gantt-today {
  font-size: 11px;
  fill: var(--td-error-color, #d54941);
}

.view-switch {
  margin-right: 4px;
}

.toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;

  &__count {
    font-size: 13px;
    color: var(--td-text-color-placeholder);
    white-space: nowrap;
  }
}

.filter-card {
  :deep(.t-card__body) {
    padding-bottom: 4px;
  }
}

.filter-label {
  font-size: 12px;
  color: var(--td-text-color-secondary, #666);
}

.toolbar__tag {
  margin-left: 8px;
  padding: 1px 6px;
  font-size: 11px;
  border-radius: 3px;
  color: var(--td-brand-color, #51697c);
  background: var(--td-brand-color-1, #eef2f6);
}

.filter-meta {
  margin-top: 6px;
  font-size: 12px;
  color: var(--td-text-color-secondary, #666);
  line-height: 20px;

  &__tag {
    margin-left: 8px;
    padding: 1px 6px;
    font-size: 11px;
    border-radius: 3px;
    color: var(--td-brand-color, #51697c);
    background: var(--td-brand-color-1, #eef2f6);
  }

  &__warn {
    margin-left: 6px;
    color: var(--td-warning-color, #e37318);
  }
}

/* 小号下划线 Tab（与四视图同款样式，但更紧凑） */
.mini-tabs {
  :deep(.t-tabs__nav-wrap) {
    border-bottom: none;
  }

  :deep(.t-tabs__nav-item) {
    height: 26px;
    padding: 0 10px;
    margin-right: 4px;
    font-size: 13px;
    line-height: 26px;
  }

  :deep(.t-tabs__nav-item.t-is-active) {
    font-weight: 600;
  }

  :deep(.t-tabs__nav-item::after) {
    height: 2px;
  }
}
</style>

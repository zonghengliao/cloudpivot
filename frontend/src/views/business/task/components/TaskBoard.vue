<template>
  <div class="board">
    <!-- 工具条：刷新 + 汇总 + 拖拽提示 -->
    <div class="board__bar">
      <t-space size="small" align="center">
        <t-button size="small" variant="outline" :loading="refreshing" @click="loadAll">
          <template #icon><t-icon name="refresh" /></template>
          刷新看板
        </t-button>
        <span class="board__hint">拖拽卡片到其它列即可改状态</span>
      </t-space>
      <span class="board__total">{{ mine ? '我的任务' : '全部可见任务' }} · 共 {{ grandTotal }} 条</span>
    </div>

    <!-- 全局错误（五列全挂 / 我的任务接口失败） -->
    <t-alert v-if="globalError" class="board__alert" theme="error" :message="globalError">
      <template #operation>
        <t-button size="small" variant="outline" @click="loadAll">重试</t-button>
      </template>
    </t-alert>

    <t-loading :loading="firstLoading" size="small">
      <div class="board__cols">
        <section
          v-for="status in STATUSES"
          :key="status"
          class="col"
          :class="{
            'col--collapsed': isCollapsed(status),
            'col--active': dropTarget === status && !isCollapsed(status),
            'col--dragging-over': dropTarget === status,
          }"
          :data-status="status"
          @dragenter.prevent="handleDragEnter(status)"
          @dragover.prevent="handleDragOver(status)"
          @dragleave="handleDragLeave(status, $event)"
          @drop.prevent="handleDrop(status)"
        >
          <!-- 列头：列名 + 任务数（+ 已取消列的折叠开关） -->
          <header class="col__head">
            <span class="col__dot" :style="{ backgroundColor: statusMeta('task', status).dotVar }" />
            <span class="col__name">{{ statusMeta('task', status).label }}</span>
            <span class="col__count" :title="colCountTitle(status)">{{ visibleTasks(status).length }}</span>
            <t-button
              v-if="status === 'CANCELLED'"
              class="col__toggle"
              size="small"
              variant="text"
              :title="collapsed ? '展开已取消列' : '折叠已取消列'"
              @click="collapsed = !collapsed"
            >
              <template #icon><t-icon :name="collapsed ? 'chevron-right' : 'chevron-left'" /></template>
            </t-button>
          </header>

          <!-- 折叠态：只留一条窄条，提示可展开 -->
          <div v-if="isCollapsed(status)" class="col__folded" @click="collapsed = false">
            <span class="col__folded-text">已折叠 {{ visibleTasks(status).length }} 条</span>
          </div>

          <!-- 展开态：卡片 + 空态 + 加载更多 -->
          <div v-else class="col__body">
            <t-alert v-if="column(status).error" theme="error" :message="column(status).error" class="col__error">
              <template #operation>
                <t-button size="small" variant="outline" @click="reloadColumn(status)">重试</t-button>
              </template>
            </t-alert>

            <!-- 父卡片 + 内嵌子任务清单（E6c）：子任务不单独成卡 -->
            <div
              v-for="task in visibleTasks(status)"
              :key="task.id"
              class="card-wrap"
              :class="{ 'card-wrap--has-kids': hasSubtasks(task) }"
            >
              <BoardCard
                :task="task"
                :pending="pendingIds.has(task.id)"
                @drag-start="handleCardDragStart"
                @drag-end="handleCardDragEnd"
                @open="(item) => emit('open', item)"
              />

              <div
                v-if="hasSubtasks(task)"
                class="subtasks"
                draggable="false"
                @click.stop
                @mousedown.stop
                @dragstart.prevent.stop
              >
                <div class="subtasks__head">
                  <span class="subtasks__label">子任务</span>
                  <span class="subtasks__stat">{{ doneCountOf(task) }}/{{ childTotalOf(task) }}</span>
                </div>

                <div v-if="stOf(task)?.loading" class="subtasks__hint">
                  <t-loading size="12px" />
                  <span>加载子任务…</span>
                </div>

                <button v-else-if="stOf(task)?.error" type="button" class="subtasks__hint subtasks__hint--error" @click.stop="retrySubtasks(task)">
                  {{ stOf(task)?.error }}，点此重试
                </button>

                <template v-else>
                  <ul class="subtasks__list">
                    <li
                      v-for="child in shownChildren(task)"
                      :key="child.id"
                      class="subtasks__row"
                      :class="{ 'subtasks__row--done': isDone(child), 'subtasks__row--pending': childPending.has(child.id) }"
                      :title="`${child.task_no} ${child.title}`"
                    >
                      <span
                        class="subtasks__check"
                        role="checkbox"
                        :aria-checked="isDone(child)"
                        :aria-label="`${isDone(child) ? '已完成' : '标记完成'}：${child.title}`"
                        tabindex="0"
                        @click.stop="toggleChildDone(task, child)"
                        @keydown.enter.stop.prevent="toggleChildDone(task, child)"
                        @keydown.space.stop.prevent="toggleChildDone(task, child)"
                      >{{ isDone(child) ? '☑' : '☐' }}</span>
                      <span class="subtasks__name" @click.stop="emit('open', child)">{{ child.title }}</span>
                      <span class="subtasks__progress">{{ clampedProgress(child) }}%</span>
                    </li>
                  </ul>

                  <button v-if="restCount(task) > 0" type="button" class="subtasks__more" @click.stop="emit('open', task)">
                    还有 {{ restCount(task) }} 个…
                  </button>

                  <p v-if="!shownChildren(task).length" class="subtasks__hint">子任务明细无权查看（共 {{ childTotalOf(task) }} 个）</p>
                </template>
              </div>
            </div>

            <!-- 空态：淡色虚线框 -->
            <div v-if="!visibleTasks(status).length && !column(status).loading && !column(status).error" class="col__empty">
              暂无任务
            </div>

            <!-- 首屏加载 -->
            <div v-if="column(status).loading && !column(status).tasks.length" class="col__loading">
              <t-loading size="16px" />
            </div>

            <!-- 加载更多（page + 1，每列 50 条一屏） -->
            <div
              v-if="column(status).tasks.length && column(status).tasks.length < column(status).total"
              class="col__more"
            >
              <t-button size="small" variant="text" :loading="column(status).loading" @click="loadMore(status)">
                加载更多（还有 {{ column(status).total - column(status).tasks.length }} 条）
              </t-button>
            </div>
          </div>
        </section>
      </div>
    </t-loading>

    <!-- 打回必填原因（REVIEW → DOING）：不填不发请求，取消则卡片不动 -->
    <t-dialog
      v-model:visible="reasonVisible"
      header="打回原因"
      width="480px"
      :footer="false"
      :close-on-overlay-click="false"
      @close="cancelReason"
    >
      <p class="reason-tip">
        任务「{{ reasonTarget?.task.title }}」从<b>待验收</b>打回<b>进行中</b>，必须填写原因（至少 5 个字），
        原因会记入任务时间线。
      </p>
      <t-textarea
        v-model="reasonText"
        :maxlength="500"
        show-limit
        :autosize="{ minRows: 3, maxRows: 6 }"
        placeholder="例如：验收标准未对齐，单测未覆盖"
      />
      <p v-if="reasonError" class="reason-error">{{ reasonError }}</p>
      <div class="reason-actions">
        <t-button variant="outline" @click="cancelReason">取消</t-button>
        <t-button theme="primary" :loading="reasonSubmitting" @click="submitReason">提交打回</t-button>
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 任务看板（T7b）—— 五列 + 原生拖拽改状态 + 乐观回弹 + 打回必填原因。
 *
 * **E6c**：卡片内嵌子任务清单（`child_count > 0` → 异步 `listSubtasks`，
 * 最多 5 行 + 「还有 N 个…」，子任务行不参与拖拽、点 `☐` 直接标完成）。
 * 明细口径见文件后半段「E6c：内嵌子任务清单」注释。
 *
 * # 数据
 *
 * 后端 `GET /business/task/page` 的 `status` 只收**单值**（`dto::normalize_status`），
 * 所以这里是 **5 次并发请求**（每列一次，`page_size=50`），而不是「一次多状态查询」。
 * 「我的任务」模式走 `GET /business/task/my`（一次拉全量五组），本地按状态分组，
 * 该接口不支持关键词 / 优先级 / 截止日参数，故这三个筛选在本地过滤。
 *
 * # 拖拽（红线）
 *
 * 1. **不引第三方库**：HTML5 原生 `draggable` + `dragstart/dragover/drop`；
 * 2. **乐观更新 + 回弹**：drop 时先把卡片挪到目标列、改两边计数，再发
 *    `PATCH /api/business/task/{id}/status`；失败用 drop 前的快照**原样还原**两列
 *    （连顺序一起还原，避免「回弹后卡片换了位置」）；
 * 3. **按错误码说话**：`63010` 非法流转 / `63011` 无权 / `63013` 缺原因，
 *    不给「操作失败」这种糊话；`63013` 会直接弹窗要求补原因后重试；
 * 4. **打回特例**：`REVIEW → DOING` **先弹窗填原因**（≥5 字），取消则卡片一动不动。
 *
 * # 排序（列内）
 *
 * `priority ASC`（P0 最前）→ `due_date ASC`（无期限排最后）→ `created_at DESC`。
 * 乐观插入也走同一个 `sortTasks`，所以「看起来落在哪儿」就是「刷新后落在哪儿」。
 */
import { computed, onMounted, reactive, ref, watch } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import BoardCard from './BoardCard.vue';
import { applyStatusReceipt } from './board-receipt';
import { ApiError } from '@/api/request';
import {
  TASK_ERROR_CODE,
  changeTaskStatus,
  listMyTasks,
  listSubtasks,
  pageTasks,
  type TaskCard,
  type TaskStatusChangeResult,
} from '@/api/business/task-api';
import { statusMeta } from '@/constants/status-map';

/** 看板列顺序（= 状态机推进顺序，已取消固定在最右且默认折叠） */
const STATUSES = ['TODO', 'DOING', 'REVIEW', 'DONE', 'CANCELLED'] as const;
/** 看板状态字面量 */
type BoardStatus = (typeof STATUSES)[number];

/** 每列一屏条数 */
const PAGE_SIZE = 50;

/** 优先级排序权重（P0 最前；未知值排最后，不抛错） */
const PRIORITY_WEIGHT: Record<string, number> = { P0: 0, P1: 1, P2: 2, P3: 3 };

/** 单列状态 */
interface ColumnState {
  /** 已加载的任务（始终有序） */
  tasks: TaskCard[];
  /** 后端总数（用于判断「还能加载更多」） */
  total: number;
  /** 已加载到第几页 */
  page: number;
  /** 本列请求中 */
  loading: boolean;
  /** 本列错误文案（非空即渲染列内错误态 + 重试） */
  error: string;
}

/** 看板筛选口径（由父页面在「查询」时传入） */
const props = withDefaults(
  defineProps<{
    /** 标题关键词 */
    keyword?: string;
    /** 优先级（`P0`–`P3`） */
    priority?: string;
    /** 负责人 id */
    ownerId?: string;
    /** 截止日下界 `YYYY-MM-DD` */
    dueFrom?: string;
    /** 截止日上界 `YYYY-MM-DD` */
    dueTo?: string;
    /** 只看我的任务 */
    mine?: boolean;
  }>(),
  {
    keyword: '',
    priority: '',
    ownerId: '',
    dueFrom: '',
    dueTo: '',
    mine: false,
  },
);

const emit = defineEmits<{
  /** 点卡片 → 打开详情抽屉（由父页面持有抽屉） */
  (event: 'open', task: TaskCard): void;
  /** 每次重新加载后回报总条数（父页面工具条展示用） */
  (event: 'loaded', total: number): void;
}>();

/** 五列的数据容器 */
const board = reactive<Record<BoardStatus, ColumnState>>(createColumns());
/** 已取消列折叠（默认折叠） */
const collapsed = ref(true);
/** 首屏加载中（整板遮罩） */
const firstLoading = ref(false);
/** 全局错误（五列全失败 / 我的任务接口失败） */
const globalError = ref('');
/** 刷新按钮 loading（复用首屏遮罩会闪，故单独一个） */
const refreshing = ref(false);
/** 正在拖的任务与原列 */
const dragging = ref<{ task: TaskCard; from: BoardStatus } | null>(null);
/** 当前高亮的落点列 */
const dropTarget = ref<BoardStatus | null>(null);
/** 正在等后端确认的任务 id（禁止二次拖动 + 卡片显示 loading） */
const pendingIds = ref<Set<string>>(new Set());

/** 打回原因弹窗 */
const reasonVisible = ref(false);
/** 打回原因文本 */
const reasonText = ref('');
/** 打回原因校验错误 */
const reasonError = ref('');
/** 打回原因提交中 */
const reasonSubmitting = ref(false);
/** 打回目标（任务 + 原列 + 目标列） */
const reasonTarget = ref<{ task: TaskCard; from: BoardStatus; to: BoardStatus } | null>(null);

/** 五列总条数 */
const grandTotal = computed(() => STATUSES.reduce((sum, status) => sum + column(status).total, 0));

// ======================================================================
// E6c：内嵌子任务清单
//
// 设计（老大 2026-10-30 认可）：**子任务不单独成卡**，挂在父卡片下方的一小块清单里。
//
// 三条口径：
// 1. 数据来源：看板接口的 `child_count > 0` → 异步 `listSubtasks(parentId)`；
//    单个父任务失败不影响卡片本身（卡片照常可拖，清单位置给「重试」）。
// 2. 最多显示 5 条，其余收成「还有 N 个…」（点它打开父任务详情）。
// 3. 子任务行**不参与拖拽**（清单是卡片的兄弟节点，且 `draggable=false` +
//    `dragstart.prevent.stop`）—— 只有父卡片能拖；拖父卡到别的列时，清单跟着卡片走，
//    视觉上就是「子任务跟随父任务」。
//
// 「孤儿子任务」的处理：`TaskCard` 里没有 `parent_id`（E6a 的卡片形状就没给），
// 所以只能**反推** —— 谁的 id 出现在某个父卡片的子任务清单里，谁就是它的孩子，
// 从所在列里摘掉（否则同一张卡会在父卡片里和列里各出现一次）。
// 父任务被筛选掉时子任务的清单拉不到，它依旧作为普通卡片显示（卡面边界允许的那一支）。
// ======================================================================

/** 单个父任务的子任务清单状态 */
interface SubtaskState {
  /** 我有权看的子任务（后端已按可见性过滤） */
  list: TaskCard[];
  /** 后端给的总数（**含我看不见的**，不能拿 list.length 当总数） */
  total: number;
  /** 拉取中 */
  loading: boolean;
  /** 错误文案（非空 → 清单位置给重试按钮） */
  error: string;
}

/** 清单里最多显示几条（超出显示「还有 N 个…」） */
const SUBTASK_MAX_ROWS = 5;
/** 并发上限：看板最多 5 列 × 50 条，全量并发会打爆浏览器连接数 */
const SUBTASK_CONCURRENCY = 4;

/** parentId → 子任务清单状态（按需拉取，页面刷新时清空重拉） */
const subtasks = reactive<Record<string, SubtaskState>>({});
/** 待拉取的队列与去重表 */
const subtaskQueue: string[] = [];
const subtaskQueued = new Set<string>();
/** 正在拉取的数量（并发闸门） */
let subtaskRunning = 0;
/** 正在标完成中的子任务 id（防连点） */
const childPending = ref<Set<string>>(new Set());

/** 取子任务清单状态（可能还没拉过） */
function stOf(task: TaskCard): SubtaskState | undefined {
  return subtasks[task.id];
}

/** 父任务是否有子任务（有才渲染清单块） */
function hasSubtasks(task: TaskCard): boolean {
  return (task.child_count ?? 0) > 0;
}

/** 子任务总数（后端 total 优先，兜底用卡片的 child_count） */
function childTotalOf(task: TaskCard): number {
  return Math.max(task.child_count ?? 0, stOf(task)?.total ?? 0);
}

/** 已完成子任务数（可见清单里的 + 卡片上的 child_done_count，取大值避免「3/1」） */
function doneCountOf(task: TaskCard): number {
  const loaded = stOf(task)?.list.filter(isDone).length ?? 0;
  return Math.min(childTotalOf(task), Math.max(loaded, task.child_done_count ?? 0));
}

/** 是否已完成 */
function isDone(child: TaskCard): boolean {
  return (child.status ?? '').toUpperCase() === 'DONE';
}

/** 进度收敛到 0~100 */
function clampedProgress(child: TaskCard): number {
  return Math.min(100, Math.max(0, Math.round(child.progress ?? 0)));
}

/** 清单里显示的若干条（最多 5 条） */
function shownChildren(task: TaskCard): TaskCard[] {
  return (stOf(task)?.list ?? []).slice(0, SUBTASK_MAX_ROWS);
}

/** 还有几个没显示出来（用于「还有 N 个…」；0 = 不显示） */
function restCount(task: TaskCard): number {
  const state = stOf(task);
  if (!state) {
    return 0;
  }
  return Math.max(0, Math.max(state.total, state.list.length) - SUBTASK_MAX_ROWS);
}

/** 已知的子任务 id（= 已经内嵌进父卡片的那些） */
const knownChildIds = computed<Set<string>>(() => {
  const set = new Set<string>();
  for (const key of Object.keys(subtasks)) {
    for (const child of subtasks[key].list) {
      set.add(child.id);
    }
  }
  return set;
});

/** 该列真正渲染的卡片（把已内嵌的子任务摘掉，避免「一卡两现」） */
function visibleTasks(status: BoardStatus): TaskCard[] {
  const nested = knownChildIds.value;
  return board[status].tasks.filter((task) => !nested.has(task.id));
}

/** 列头计数气泡的 tooltip：说清「列里显示 N 条」与「其中 M 个子任务已内嵌」 */
function colCountTitle(status: BoardStatus): string {
  const nested = board[status].tasks.length - visibleTasks(status).length;
  if (nested <= 0) {
    return `${statusMeta('task', status).label}：${board[status].total} 条`;
  }
  return `${statusMeta('task', status).label}：${visibleTasks(status).length} 张卡片（另有 ${nested} 个子任务内嵌在父卡片里）`;
}

/** 对齐当前板上的卡片与子任务清单：清掉已离开的缓存、给新出现的父卡片排队 */
function syncSubtasks(): void {
  const alive = new Set<string>();
  const need: string[] = [];
  for (const status of STATUSES) {
    for (const task of board[status].tasks) {
      alive.add(task.id);
      if (hasSubtasks(task)) {
        need.push(task.id);
      }
    }
  }
  for (const key of Object.keys(subtasks)) {
    if (!alive.has(key)) {
      delete subtasks[key];
    }
  }
  for (const id of need) {
    enqueueSubtasks(id);
  }
}

/** 入队（已有缓存 / 已在队列里的不重复拉） */
function enqueueSubtasks(parentId: string): void {
  if (subtasks[parentId] || subtaskQueued.has(parentId)) {
    return;
  }
  subtaskQueued.add(parentId);
  subtaskQueue.push(parentId);
  void drainSubtasks();
}

/** 并发闸门：最多 `SUBTASK_CONCURRENCY` 个子任务请求同时在飞 */
async function drainSubtasks(): Promise<void> {
  while (subtaskRunning < SUBTASK_CONCURRENCY && subtaskQueue.length > 0) {
    const parentId = subtaskQueue.shift() as string;
    subtaskRunning += 1;
    void fetchSubtasks(parentId).then(() => {
      subtaskRunning -= 1;
      subtaskQueued.delete(parentId);
      void drainSubtasks();
    });
  }
}

/** 拉一个父任务的子任务清单（失败只标记该卡片，不影响整板） */
async function fetchSubtasks(parentId: string): Promise<void> {
  const previous = subtasks[parentId];
  subtasks[parentId] = {
    list: previous?.list ?? [],
    total: previous?.total ?? 0,
    loading: true,
    error: '',
  };
  try {
    const data = await listSubtasks(parentId);
    subtasks[parentId] = {
      list: data.list ?? [],
      total: data.total ?? 0,
      loading: false,
      error: '',
    };
  } catch (error) {
    subtasks[parentId] = { list: [], total: 0, loading: false, error: errorText(error) };
  }
}

/** 清单加载失败后重试（先删缓存让 `enqueueSubtasks` 认可这次重试） */
function retrySubtasks(task: TaskCard): void {
  delete subtasks[task.id];
  subtaskQueued.delete(task.id);
  enqueueSubtasks(task.id);
}

/**
 * 点 `☐` 把子任务标完成（`PATCH /api/business/task/{id}/status`）。
 *
 * 只发**一次**流转：状态机白名单由后端把关（TODO 直接到 DONE 属于跳级 → 400 + 63010），
 * 前端不替用户「偷偷走两步」——失败时按业务码给具体文案（见 `transitionErrorText`）。
 * 成功后用回执校正子任务自身 + 父任务被重算的进度。
 */
async function toggleChildDone(parent: TaskCard, child: TaskCard): Promise<void> {
  if (isDone(child)) {
    MessagePlugin.info('该子任务已完成；如需重新开工，请打开任务详情做状态流转');
    return;
  }
  if (childPending.value.has(child.id)) {
    return;
  }
  childPending.value = new Set(childPending.value).add(child.id);
  let result: TaskStatusChangeResult;
  try {
    result = await changeTaskStatus(child.id, 'DONE');
  } catch (error) {
    MessagePlugin.error(transitionErrorText(error));
    return;
  } finally {
    const next = new Set(childPending.value);
    next.delete(child.id);
    childPending.value = next;
  }

  // 服务端已确认：回执走同一套兜底（出错只记日志，不当作请求失败）
  const receipt = applyStatusReceipt({
    targetTasks: [child],
    taskId: child.id,
    receipt: result,
    fallbackStatus: 'DONE',
    boardTasks: STATUSES.map((status) => board[status].tasks),
  });
  if (receipt.error) {
    console.error('[task-board] 子任务回执处理失败（已按「服务端成功」处理）', receipt.error);
  }
  parent.child_done_count = doneCountOf(parent);
  MessagePlugin.success('子任务已标记完成');
}

/** 建一份干净的五列容器 */
function createColumns(): Record<BoardStatus, ColumnState> {
  const out = {} as Record<BoardStatus, ColumnState>;
  for (const status of STATUSES) {
    out[status] = { tasks: [], total: 0, page: 1, loading: false, error: '' };
  }
  return out;
}

/** 取某列状态（避免模板里反复写 `board[status]`） */
function column(status: BoardStatus): ColumnState {
  return board[status];
}

/** 是否折叠（只有已取消列会折叠） */
function isCollapsed(status: BoardStatus): boolean {
  return status === 'CANCELLED' && collapsed.value;
}

/**
 * 列内排序：优先级 → 截止日 → 创建时间倒序。
 *
 * `YYYY-MM-DD` 直接字符串比较即时间顺序（同格式定长），不必转 Date；
 * 无截止日排在本列最后（`due_date ASC` 的 NULLS LAST 语义）。
 */
function sortTasks(tasks: TaskCard[]): TaskCard[] {
  return [...tasks].sort((a, b) => {
    const weightA = PRIORITY_WEIGHT[(a.priority ?? '').toUpperCase()] ?? 99;
    const weightB = PRIORITY_WEIGHT[(b.priority ?? '').toUpperCase()] ?? 99;
    if (weightA !== weightB) {
      return weightA - weightB;
    }
    const dueA = a.due_date ?? '';
    const dueB = b.due_date ?? '';
    if (dueA !== dueB) {
      if (!dueA) {
        return 1;
      }
      if (!dueB) {
        return -1;
      }
      return dueA < dueB ? -1 : 1;
    }
    const createdA = Date.parse(a.created_at ?? '') || 0;
    const createdB = Date.parse(b.created_at ?? '') || 0;
    return createdB - createdA;
  });
}

/** 分页查询的公共筛选参数（`status` 由调用方补） */
function baseQuery(): Record<string, string> {
  return {
    keyword: props.keyword ?? '',
    priority: props.priority ?? '',
    owner_id: props.ownerId ?? '',
    due_from: props.dueFrom ?? '',
    due_to: props.dueTo ?? '',
  };
}

/** 错误文案（优先按业务码翻译，兜底用后端 msg） */
function errorText(error: unknown): string {
  if (error instanceof ApiError) {
    if (!error.message) {
      return '任务加载失败，请稍后重试';
    }
    return error.message;
  }
  return '任务加载失败，请稍后重试';
}

/**
 * 状态流转失败的**具体文案**（红线：不许只说「操作失败」）。
 *
 * 判据是业务码而不是 HTTP 状态：`api/request.ts` 已把 HTTP 层错误规整成
 * `ApiError`，只保留后端 `code`。
 */
function transitionErrorText(error: unknown): string {
  if (error instanceof ApiError) {
    switch (error.code) {
      case TASK_ERROR_CODE.TRANSITION_INVALID:
        return '该状态流转不被允许（如 TODO 不能直接到 DONE）';
      case TASK_ERROR_CODE.STATUS_FORBIDDEN:
        return '你没有权限执行该流转（待验收需验收人操作）';
      case TASK_ERROR_CODE.REJECT_REASON_REQUIRED:
        return '打回必须填写原因（至少 5 个字）';
      case TASK_ERROR_CODE.HAS_OPEN_CHILDREN:
        return '该任务还有未完成的子任务，不能标记完成';
      case TASK_ERROR_CODE.NOT_FOUND:
        return '任务不存在或你已无权查看，看板已刷新';
      default:
        return error.message || '状态流转失败，请稍后重试';
    }
  }
  return '状态流转失败，请检查网络后重试';
}

/** 全量加载（五列并发 / 我的任务单次） */
async function loadAll(): Promise<void> {
  const isRefresh = board.TODO.tasks.length > 0 || grandTotal.value > 0;
  if (isRefresh) {
    refreshing.value = true;
  } else {
    firstLoading.value = true;
  }
  globalError.value = '';
  for (const status of STATUSES) {
    board[status].tasks = [];
    board[status].total = 0;
    board[status].page = 1;
    board[status].error = '';
    board[status].loading = true;
  }
  try {
    if (props.mine) {
      await loadMine();
    } else {
      await loadColumns();
    }
  } finally {
    for (const status of STATUSES) {
      board[status].loading = false;
    }
    firstLoading.value = false;
    refreshing.value = false;
    emit('loaded', grandTotal.value);
  }
}

/** 五列并发拉取（每列一次 `page_size=50`） */
async function loadColumns(): Promise<void> {
  const results = await Promise.all(
    STATUSES.map(async (status) => {
      try {
        const data = await pageTasks({ page: 1, page_size: PAGE_SIZE, status, ...baseQuery() });
        return { status, data, message: '' };
      } catch (error) {
        return { status, data: null, message: errorText(error) };
      }
    }),
  );
  let failed = 0;
  for (const item of results) {
    const state = board[item.status];
    if (item.data) {
      state.tasks = sortTasks(item.data.list);
      state.total = item.data.total;
      state.page = item.data.page ?? 1;
    } else {
      failed += 1;
      state.error = item.message;
    }
  }
  if (failed === STATUSES.length) {
    globalError.value = '看板数据加载失败：请检查后端服务是否可用，然后点「重试」';
  }
  syncSubtasks();
}

/**
 * 「我的任务」模式：`GET /business/task/my` 一次拉五组，本地按状态归位。
 *
 * 该接口不吃 `keyword/priority/due_*`，所以这三个筛选在本地过一遍，
 * 保证「看板看到的」与「列表页看到的分组」口径一致。
 */
async function loadMine(): Promise<void> {
  try {
    const data = await listMyTasks();
    const flattened = [
      ...data.overdue,
      ...data.today,
      ...data.this_week,
      ...data.later,
      ...data.no_due,
    ];
    const keyword = (props.keyword ?? '').trim().toLowerCase();
    const priority = (props.priority ?? '').trim().toUpperCase();
    const filtered = flattened.filter((task) => {
      if (keyword && !`${task.title} ${task.task_no}`.toLowerCase().includes(keyword)) {
        return false;
      }
      if (priority && (task.priority ?? '').toUpperCase() !== priority) {
        return false;
      }
      const due = task.due_date ?? '';
      if (props.dueFrom && (!due || due < props.dueFrom)) {
        return false;
      }
      if (props.dueTo && (!due || due > props.dueTo)) {
        return false;
      }
      return true;
    });
    for (const status of STATUSES) {
      const list = sortTasks(filtered.filter((task) => (task.status ?? '').toUpperCase() === status));
      board[status].tasks = list;
      board[status].total = list.length;
      board[status].page = 1;
    }
    syncSubtasks();
  } catch (error) {
    globalError.value = errorText(error);
  }
}

/** 单列重试（列内错误态的「重试」） */
async function reloadColumn(status: BoardStatus): Promise<void> {
  const state = board[status];
  state.loading = true;
  state.error = '';
  try {
    const data = await pageTasks({ page: 1, page_size: PAGE_SIZE, status, ...baseQuery() });
    state.tasks = sortTasks(data.list);
    state.total = data.total;
    state.page = data.page ?? 1;
  } catch (error) {
    state.error = errorText(error);
  } finally {
    state.loading = false;
    syncSubtasks();
    emit('loaded', grandTotal.value);
  }
}

/** 加载更多（page + 1） */
async function loadMore(status: BoardStatus): Promise<void> {
  const state = board[status];
  if (state.loading || state.tasks.length >= state.total) {
    return;
  }
  state.loading = true;
  try {
    const data = await pageTasks({
      page: state.page + 1,
      page_size: PAGE_SIZE,
      status,
      ...baseQuery(),
    });
    const seen = new Set(state.tasks.map((task) => task.id));
    const appended = data.list.filter((task) => !seen.has(task.id));
    state.tasks = sortTasks([...state.tasks, ...appended]);
    state.total = data.total;
    state.page = data.page ?? state.page + 1;
  } catch (error) {
    MessagePlugin.error(errorText(error));
  } finally {
    state.loading = false;
    syncSubtasks();
  }
}

/** 起拖：记下来源列 */
function handleCardDragStart(task: TaskCard): void {
  const status = (task.status ?? '').toUpperCase() as BoardStatus;
  if (!STATUSES.includes(status)) {
    return;
  }
  dragging.value = { task, from: status };
}

/** 拖结束：清空拖拽态（drop 未命中任何列时也要清） */
function handleCardDragEnd(): void {
  dragging.value = null;
  dropTarget.value = null;
}

/** 进入某一列：高亮 + 已取消列自动展开（否则没法往里放） */
function handleDragEnter(status: BoardStatus): void {
  if (!dragging.value) {
    return;
  }
  dropTarget.value = status;
  if (status === 'CANCELLED' && collapsed.value) {
    collapsed.value = false;
  }
}

/** 在列上移动：保持高亮并告诉浏览器这是「移动」 */
function handleDragOver(status: BoardStatus): void {
  if (!dragging.value) {
    return;
  }
  dropTarget.value = status;
}

/** 离开列：只有真的离开整列（进入子元素不算）才取消高亮 */
function handleDragLeave(status: BoardStatus, event: DragEvent): void {
  const current = event.currentTarget as HTMLElement | null;
  const next = event.relatedTarget as Node | null;
  if (current && next && current.contains(next)) {
    return;
  }
  if (dropTarget.value === status) {
    dropTarget.value = null;
  }
}

/** 松手：同列不动；打回先弹窗；其余走乐观更新 */
function handleDrop(status: BoardStatus): void {
  const drag = dragging.value;
  dragging.value = null;
  dropTarget.value = null;
  if (!drag) {
    return;
  }
  const { task, from } = drag;
  if (from === status) {
    return;
  }
  if (from === 'REVIEW' && status === 'DOING') {
    openReasonDialog(task, from, status);
    return;
  }
  void moveTask(task, from, status);
}

/** 打开打回原因弹窗（取消则卡片不动） */
function openReasonDialog(task: TaskCard, from: BoardStatus, to: BoardStatus): void {
  reasonTarget.value = { task, from, to };
  reasonText.value = '';
  reasonError.value = '';
  reasonVisible.value = true;
}

/** 取消打回：卡片保持原样（此时还没做任何乐观更新） */
function cancelReason(): void {
  reasonVisible.value = false;
  reasonTarget.value = null;
  reasonError.value = '';
}

/** 提交打回：本地校验 ≥5 字，通过才发请求 */
async function submitReason(): Promise<void> {
  const target = reasonTarget.value;
  if (!target) {
    return;
  }
  const text = reasonText.value.trim();
  if (text.length < 5) {
    reasonError.value = `请填写打回原因，至少 5 个字（当前 ${text.length} 个）`;
    return;
  }
  reasonSubmitting.value = true;
  try {
    reasonVisible.value = false;
    reasonTarget.value = null;
    await moveTask(target.task, target.from, target.to, text);
  } finally {
    reasonSubmitting.value = false;
  }
}

/**
 * 乐观更新 + 回弹。
 *
 * 先动 UI（挪卡片 + 改两边计数），请求落地后再用回执校正 `progress`
 * （状态联动会抬进度）；**只有请求本身失败**才用 drop 前的两列快照整体还原。
 *
 * # 两条边界（2026-10-30 12:20 流程3 偶发红后收紧）
 *
 * 1. `try/catch/finally` 只罩住 **`changeTaskStatus` 这一次请求**：catch 里才回弹。
 * 2. 请求一旦拿到 200（`code === 0`），后续「回执处理」（校正状态/进度、同步父任务进度、
 *    弹成功提示）走 `applyStatusReceipt`，它自带兜底、**不抛**；万一真出了岔子只记日志，
 *    **绝不回滚** —— 服务端已经落地的状态不允许被前端回退成相反的样子。
 */
async function moveTask(task: TaskCard, from: BoardStatus, to: BoardStatus, reason?: string): Promise<void> {
  const source = board[from];
  const target = board[to];
  if (!source || !target) {
    return;
  }
  // 回弹快照（含顺序与计数）
  const snapshot = {
    sourceTasks: [...source.tasks],
    sourceTotal: source.total,
    targetTasks: [...target.tasks],
    targetTotal: target.total,
  };

  // ① 乐观更新
  source.tasks = source.tasks.filter((item) => item.id !== task.id);
  source.total = Math.max(0, source.total - 1);
  target.tasks = sortTasks([{ ...task, status: to }, ...target.tasks]);
  target.total += 1;
  pendingIds.value = new Set(pendingIds.value).add(task.id);
  emit('loaded', grandTotal.value);

  let result: TaskStatusChangeResult;
  try {
    result = await changeTaskStatus(task.id, to, reason);
  } catch (error) {
    // ④ 回弹：请求没落地 → 两列原样还原
    source.tasks = snapshot.sourceTasks;
    source.total = snapshot.sourceTotal;
    target.tasks = snapshot.targetTasks;
    target.total = snapshot.targetTotal;
    if (error instanceof ApiError && error.code === TASK_ERROR_CODE.REJECT_REASON_REQUIRED) {
      MessagePlugin.warning('打回必须填写原因（至少 5 个字）');
      openReasonDialog(task, from, to);
    } else {
      MessagePlugin.error(transitionErrorText(error));
    }
    emit('loaded', grandTotal.value);
    return;
  } finally {
    const next = new Set(pendingIds.value);
    next.delete(task.id);
    pendingIds.value = next;
  }

  // ② 服务端已确认：用回执校正（进度联动 / 状态归一），出错只记日志、不回滚
  const receipt = applyStatusReceipt({
    targetTasks: target.tasks,
    taskId: task.id,
    receipt: result,
    fallbackStatus: to,
    boardTasks: STATUSES.map((status) => board[status].tasks),
  });
  if (receipt.error) {
    console.error('[task-board] 状态流转回执处理失败（已按「服务端成功」处理，卡片留在目标列）', receipt.error);
  }
  // ③ 父任务进度被重算 → 板上同 id 的卡片同步（可能不在本屏，找不到就算了）
  MessagePlugin.success(`已移动到「${statusMeta('task', to).label}」`);
}

/** 筛选变化 → 重新加载（父页面「查询 / 重置 / 我的任务开关」都会动这些 props） */
watch(
  () => [props.keyword, props.priority, props.ownerId, props.dueFrom, props.dueTo, props.mine],
  () => {
    void loadAll();
  },
);

/** 打回原因一改就把校验错误清掉（避免「补够字数了还红着」） */
watch(reasonText, () => {
  if (reasonError.value) {
    reasonError.value = '';
  }
});

/** 父页面保存 / 删除任务后调用：重新拉一遍 */
function reload(): void {
  void loadAll();
}

defineExpose({ reload });

onMounted(() => {
  void loadAll();
});
</script>

<style scoped lang="less">
.board {
  display: flex;
  flex-direction: column;
  gap: 12px;

  &__bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }

  &__hint,
  &__total {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__alert {
    margin-bottom: 4px;
  }

  &__cols {
    display: flex;
    gap: 12px;
    align-items: flex-start;
    overflow-x: auto;
    padding-bottom: 4px;
  }
}

// ==================== E6c：卡片 + 内嵌子任务清单 ====================
// 清单是卡片的**兄弟节点**（不是卡片内部）：这样点子任务不会冒泡到卡片的
// click / dragstart，天然满足「子任务行点击不触发卡片拖拽」。
.card-wrap {
  display: flex;
  flex-direction: column;
  min-width: 0;
}

// 有子任务时卡片下边框去掉、与清单贴合成一整块（视觉上仍是一张卡）
.card-wrap--has-kids :deep(.board-card) {
  border-bottom-color: transparent;
  border-radius: var(--td-radius-medium) var(--td-radius-medium) 0 0;
}

.subtasks {
  padding: 6px 8px 8px;
  cursor: default;
  user-select: none;
  background: var(--td-bg-color-container-hover);
  border: 1px solid var(--td-component-stroke);
  border-top: 1px dashed var(--td-component-stroke);
  border-radius: 0 0 var(--td-radius-medium) var(--td-radius-medium);

  &__head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 6px;
    margin-bottom: 4px;
  }

  &__label {
    font-size: 11px;
    font-weight: 600;
    color: var(--td-text-color-secondary);
  }

  &__stat {
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    color: var(--td-text-color-placeholder);
  }

  &__list {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  &__row {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 1px 2px;
    border-radius: var(--td-radius-small, 3px);

    &:hover {
      background: var(--td-bg-color-container);
    }
  }

  &__check {
    flex-shrink: 0;
    font-size: 12px;
    line-height: 16px;
    color: var(--td-brand-color);
    cursor: pointer;
  }

  &__name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    font-size: 12px;
    line-height: 16px;
    color: var(--td-text-color-primary);
    text-overflow: ellipsis;
    white-space: nowrap;
    cursor: pointer;

    &:hover {
      color: var(--td-brand-color);
      text-decoration: underline;
    }
  }

  &__progress {
    flex-shrink: 0;
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    color: var(--td-text-color-placeholder);
  }

  // 已完成：整体降透明度 + 标题划掉（与父卡「终态降透明」同一套语言）
  &__row--done {
    opacity: 0.62;

    .subtasks__name {
      color: var(--td-text-color-placeholder);
      text-decoration: line-through;
    }

    .subtasks__check {
      color: var(--td-success-color);
    }
  }

  &__row--pending {
    cursor: progress;
    opacity: 0.55;
  }

  &__more {
    display: block;
    margin-top: 4px;
    padding: 0;
    font-size: 11px;
    color: var(--td-brand-color);
    background: none;
    border: 0;
    cursor: pointer;

    &:hover {
      text-decoration: underline;
    }
  }

  &__hint {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0;
    font-size: 11px;
    color: var(--td-text-color-placeholder);
  }

  &__hint--error {
    padding: 0;
    text-align: left;
    background: none;
    border: 0;
    cursor: pointer;
    color: var(--td-error-color);
  }
}

.col {
  display: flex;
  flex: 1 0 258px;
  flex-direction: column;
  gap: 8px;
  min-width: 258px;
  max-height: 74vh;
  padding: 10px;
  overflow-y: auto;
  background: var(--td-bg-color-page);
  border: 1px solid transparent;
  border-radius: var(--td-radius-medium);
  transition:
    border-color 0.15s,
    background 0.15s;

  &--collapsed {
    flex: 0 0 132px;
    min-width: 132px;
  }

  &--dragging-over {
    background: var(--td-bg-color-container-hover);
  }

  &--active {
    border-color: var(--td-brand-color);
    border-style: dashed;
  }

  &__head {
    display: flex;
    align-items: center;
    gap: 6px;
    padding-bottom: 4px;
    border-bottom: 1px solid var(--td-component-stroke);
  }

  &__dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }

  &__name {
    font-size: 13px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__count {
    min-width: 20px;
    padding: 0 6px;
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    text-align: center;
    color: var(--td-text-color-secondary);
    background: var(--td-bg-color-secondarycontainer);
    border-radius: 9px;
  }

  &__toggle {
    margin-left: auto;
  }

  &__body {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  &__folded {
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 120px;
    cursor: pointer;
    border: 1px dashed var(--td-component-stroke);
    border-radius: var(--td-radius-medium);
  }

  &__folded-text {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
    writing-mode: vertical-rl;
  }

  &__empty {
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 76px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
    border: 1px dashed var(--td-component-stroke);
    border-radius: var(--td-radius-medium);
  }

  &__loading,
  &__more {
    display: flex;
    justify-content: center;
    padding: 4px 0;
  }

  &__error {
    font-size: 12px;
  }
}

.reason-tip {
  margin: 0 0 10px;
  font-size: 13px;
  line-height: 1.6;
  color: var(--td-text-color-secondary);
}

.reason-error {
  margin: 6px 0 0;
  font-size: 12px;
  color: var(--td-error-color);
}

.reason-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 14px;
}
</style>

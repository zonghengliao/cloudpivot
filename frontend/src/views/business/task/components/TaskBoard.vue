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
            <span class="col__count">{{ column(status).total }}</span>
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
            <span class="col__folded-text">已折叠 {{ column(status).total }} 条</span>
          </div>

          <!-- 展开态：卡片 + 空态 + 加载更多 -->
          <div v-else class="col__body">
            <t-alert v-if="column(status).error" theme="error" :message="column(status).error" class="col__error">
              <template #operation>
                <t-button size="small" variant="outline" @click="reloadColumn(status)">重试</t-button>
              </template>
            </t-alert>

            <BoardCard
              v-for="task in column(status).tasks"
              :key="task.id"
              :task="task"
              :pending="pendingIds.has(task.id)"
              @drag-start="handleCardDragStart"
              @drag-end="handleCardDragEnd"
              @open="(item) => emit('open', item)"
            />

            <!-- 空态：淡色虚线框 -->
            <div v-if="!column(status).tasks.length && !column(status).loading && !column(status).error" class="col__empty">
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
import { ApiError } from '@/api/request';
import {
  TASK_ERROR_CODE,
  changeTaskStatus,
  listMyTasks,
  pageTasks,
  type ParentProgress,
  type TaskCard,
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
 * （状态联动会抬进度）；失败则用 drop 前的两列快照整体还原。
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

  try {
    const result = await changeTaskStatus(task.id, to, reason);
    // ② 用回执校正（进度联动 / 状态归一）
    const landed = target.tasks.find((item) => item.id === task.id);
    if (landed) {
      landed.status = result.status;
      landed.progress = result.progress;
    }
    // ③ 父任务进度被重算 → 板上同 id 的卡片同步（可能不在本屏，找不到就算了）
    syncParentProgress(result.parent_progress);
    MessagePlugin.success(`已移动到「${statusMeta('task', to).label}」`);
  } catch (error) {
    // ④ 回弹：两列原样还原
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
  } finally {
    const next = new Set(pendingIds.value);
    next.delete(task.id);
    pendingIds.value = next;
  }
}

/** 父任务进度重算结果 → 同步到板上已加载的卡片 */
function syncParentProgress(list: ParentProgress[]): void {
  for (const progress of list) {
    for (const status of STATUSES) {
      const hit = board[status].tasks.find((item) => item.id === progress.id);
      if (hit) {
        hit.progress = progress.progress;
      }
    }
  }
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

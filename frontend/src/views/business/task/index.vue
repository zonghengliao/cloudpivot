<template>
  <div class="task-page">
    <!-- 筛选区 -->
    <t-card :bordered="false" class="filter-card">
      <t-row :gutter="[16, 12]">
        <t-col :span="4">
          <t-input v-model="query.keyword" placeholder="任务标题 / 描述关键词" clearable @enter="handleSearch">
            <template #prefix-icon><t-icon name="search" /></template>
          </t-input>
        </t-col>
        <t-col :span="2">
          <t-select v-model="query.status" placeholder="状态" clearable :options="statusOpts" />
        </t-col>
        <t-col :span="2">
          <t-select v-model="query.priority" placeholder="优先级" clearable :options="priorityOpts" />
        </t-col>
        <t-col :span="2">
          <UserSelect v-model="query.owner_id" placeholder="负责人" :disabled="myMode" />
        </t-col>
        <t-col :span="2">
          <t-select
            v-model="query.project_id"
            :options="projectOptions"
            :loading="projectLoading"
            placeholder="所属项目"
            clearable
            filterable
            :disabled="myMode"
          />
        </t-col>
        <t-col :span="3">
          <t-date-range-picker v-model="dueRange" placeholder="截止日期范围" clearable format="YYYY-MM-DD" />
        </t-col>
        <t-col :span="3">
          <t-space>
            <t-button theme="primary" @click="handleSearch">
              <template #icon><t-icon name="search" /></template>
              查询
            </t-button>
            <t-button variant="outline" @click="handleReset">重置</t-button>
          </t-space>
        </t-col>
        <t-col :span="12">
          <t-space size="large" align="center">
            <label class="my-switch">
              <t-switch v-model="myMode" size="small" />
              <span class="my-switch__label">只看我的任务</span>
            </label>
            <span v-if="view === 'list'" class="filter-hint">
              我的任务按「逾期 / 今日 / 本周 / 以后 / 无期限」分组统计（CST 计算），共 {{ myTotal }} 条
            </span>
          </t-space>
        </t-col>
      </t-row>
    </t-card>

    <!-- 工具条：三视图切换 + 主操作（列表 / 看板共用同一份筛选条件） -->
    <t-card :bordered="false" class="toolbar-card">
      <div class="toolbar">
        <t-space align="center">
          <t-radio-group v-model="view" variant="default-filled" size="small" class="view-switch">
            <t-radio-button value="list">列表</t-radio-button>
            <t-radio-button value="board">看板</t-radio-button>
            <t-radio-button value="gantt">甘特</t-radio-button>
          </t-radio-group>
          <t-button v-privilege="[PERM.taskCreate]" theme="primary" size="small" @click="openCreate">
            <template #icon><t-icon name="add" /></template>
            新建任务
          </t-button>
          <t-button variant="outline" size="small" :loading="loading" @click="refreshCurrent">
            <template #icon><t-icon name="refresh" /></template>
            刷新
          </t-button>
        </t-space>
        <span class="toolbar__count">{{ countText }}</span>
      </div>
    </t-card>

    <!-- 列表视图 -->
    <t-card v-if="view === 'list'" :bordered="false" class="table-card">

      <!-- 可见性截断提示（后端「我参与」id 数组超 2000 上限时的降级） -->
      <t-alert
        v-if="scopeTruncated"
        class="table-alert"
        theme="warning"
        message="我参与的任务过多，本视图只显示数据范围内的任务；「我的任务」请用上方开关查看。"
      />

      <!-- 错误态：可重试，不白屏 -->
      <t-alert v-if="errorMessage" class="table-alert" theme="error" :message="errorMessage">
        <template #operation>
          <t-button size="small" variant="outline" @click="loadList">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="tablePagination"
        :hover="true"
        stripe
        @page-change="handlePageChange"
      >
        <template #task_no="{ row }">
          <span class="cell-no">{{ row.task_no }}</span>
        </template>

        <template #title="{ row }">
          <div class="cell-title">
            <t-link theme="primary" class="cell-title__text" @click="openDetail(row)">{{ row.title }}</t-link>
            <t-tag v-if="row.need_review" size="small" variant="outline" theme="default">需验收</t-tag>
          </div>
        </template>

        <template #status="{ row }">
          <StatusBadge domain="task" :status="row.status" />
        </template>

        <template #priority="{ row }">
          <PriorityTag :priority="row.priority" />
        </template>

        <template #progress="{ row }">
          <div class="cell-progress">
            <t-progress :percentage="row.progress" :label="false" theme="line" :stroke-width="6" />
            <span class="cell-progress__text">{{ row.progress }}%</span>
          </div>
        </template>

        <template #owner_name="{ row }">{{ row.owner_name || '—' }}</template>

        <template #project_name="{ row }">
          <t-link v-if="row.project_id" theme="primary" @click="openProject(row)">
            {{ row.project_name || row.project_code || '未命名项目' }}
          </t-link>
          <span v-else class="cell-project-empty">未归入项目</span>
        </template>

        <template #due_date="{ row }">
          <span :class="{ 'cell-due--overdue': isOverdue(row) }">
            {{ row.due_date || '无期限' }}
            <t-icon v-if="isOverdue(row)" name="error-circle" />
          </span>
          <span v-if="row.bucketLabel" class="cell-bucket">{{ row.bucketLabel }}</span>
        </template>

        <template #source="{ row }">
          <t-tag size="small" variant="outline" theme="default">{{ sourceLabel(row.source) }}</t-tag>
        </template>

        <template #op="{ row }">
          <t-space size="small">
            <t-link theme="primary" @click="openDetail(row)">查看</t-link>
            <t-link v-privilege="[PERM.taskUpdate]" theme="primary" @click="openEdit(row)">编辑</t-link>
            <t-link v-privilege="[PERM.taskDelete]" theme="danger" @click="confirmRemove(row)">删除</t-link>
          </t-space>
        </template>

        <template #empty>
          <div class="empty-state">
            <t-icon class="empty-state__icon" name="task" />
            <p class="empty-state__title">{{ hasFilter ? '没有符合条件的任务' : '还没有任务' }}</p>
            <p class="empty-state__desc">
              {{ hasFilter ? '换个筛选条件试试，或点「重置」看全部' : '点右上角「新建任务」创建第一条，标题敲完就能建' }}
            </p>
            <t-button v-privilege="[PERM.taskCreate]" theme="primary" variant="outline" @click="openCreate">
              新建任务
            </t-button>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- 看板视图（交互见 components/TaskBoard.vue：原生 HTML5 拖拽改状态） -->
    <t-card v-else :bordered="false" class="board-view">
      <TaskBoard
        ref="boardRef"
        :keyword="query.keyword"
        :priority="query.priority"
        :owner-id="query.owner_id"
        :due-from="dueRange?.[0] ?? ''"
        :due-to="dueRange?.[1] ?? ''"
        :mine="myMode"
        @open="openDetail"
        @loaded="handleBoardLoaded"
      />
    </t-card>

    <!-- 新建 / 编辑 -->
    <TaskForm v-model:visible="formVisible" :task-id="editingId" @saved="handleSaved" />

    <!-- 详情抽屉 -->
    <TaskDetail v-model:visible="detailVisible" :task-id="viewingId" @edit="handleEditFromDetail" @deleted="handleDeleted" />
  </div>
</template>

<script setup lang="ts">
/**
 * 工作任务 · 列表页（菜单 `/task` → `business/task/index`，入口码 `business:task:read`）。
 *
 * # 数据来源两套（同一张表，两种口径）
 *
 * - 「全部」：`GET /api/business/task/page` —— 分页 + 六个筛选维度，走数据范围 ∪ 参与关系；
 * - 「只看我的任务」：`GET /api/business/task/my` —— 按逾期/今日/本周/以后/无期限分组，
 *   本地拍平成一张表（组名用小字挂在截止日下面），不分页。
 *
 * # 列表 DTO 的字段边界（别照着设计文档写，照后端 `TaskCardRes`）
 *
 * `TaskCardRes` **没有** `creator_name` / `description` / `estimate_hours` —— 这些只在
 * 详情接口里下发。所以列表的「创建人」列改为「来源」（自建 / 被分派 / 转派），
 * 创建人在详情抽屉里看。
 *
 * # 三视图（T7b）
 *
 * `[列表] [看板] [甘特]`：列表 / 看板在 `/task` 内切换，选中态持久化到 URL query
 * （`?view=board`，刷新保持）；甘特是独立路由 `/task/gantt`，点它 `router.push` 跳走。
 * 看板（`components/TaskBoard.vue`）自己拖拽改状态 —— 这里不碰 T2 的状态机。
 */
import { computed, onMounted, reactive, ref, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { MessagePlugin, DialogPlugin } from 'tdesign-vue-next';
import dayjs from 'dayjs';
import StatusBadge from './components/StatusBadge.vue';
import PriorityTag from './components/PriorityTag.vue';
import UserSelect from './components/UserSelect.vue';
import TaskForm from './components/TaskForm.vue';
import TaskBoard from './components/TaskBoard.vue';
import TaskDetail from './detail.vue';
import { ApiError } from '@/api/request';
import {
  deleteTask,
  listMyTasks,
  pageTasks,
  type MyTaskCounts,
  type TaskCard,
  type TaskPageQuery,
} from '@/api/business/task-api';
import { pageProjects } from '@/api/business/project-api';
import { PERM } from '@/constants/permissions';
import { priorityOptions, statusOptions } from '@/constants/status-map';

/** 列表行：任务卡 + 「我的任务」分组标签（仅我的任务视图有值） */
type TaskRow = TaskCard & { bucketLabel?: string };

/** 状态 / 优先级筛选选项（文案与徽标同源，来自 status-map） */
const statusOpts = statusOptions('task');
const priorityOpts = priorityOptions();

/** 表格数据 */
const rows = ref<TaskRow[]>([]);
/** 加载中 */
const loading = ref(false);
/** 错误文案（非空即渲染错误态 + 重试） */
const errorMessage = ref('');
/** 「我参与」id 数组被后端截断（本视图只含数据范围内任务） */
const scopeTruncated = ref(false);

/** 筛选条件 */
const query = reactive<Required<Pick<TaskPageQuery, 'keyword' | 'status' | 'priority' | 'owner_id' | 'project_id'>>>({
  keyword: '',
  status: '',
  priority: '',
  owner_id: '',
  // 项目筛选：初值留空，`onMounted` 里从 URL query 读（项目页「查看项目下任务」会带过来）
  project_id: '',
});
/** 截止日期范围（`[from, to]`，`YYYY-MM-DD`） */
const dueRange = ref<string[]>([]);
/** 只看我的任务 */
const myMode = ref(false);
/** 我的任务各组条数 */
const myCounts = ref<MyTaskCounts | null>(null);

/** 分页 */
const pagination = reactive({ current: 1, pageSize: 10, total: 0, showJumper: true, pageSizeOptions: [10, 20, 50] });
/** 我的任务视图不分页 */
const tablePagination = computed(() => (myMode.value ? undefined : pagination));

/** 表单 / 详情状态 */
const formVisible = ref(false);
const editingId = ref<string | null>(null);
const detailVisible = ref(false);
const viewingId = ref<string | null>(null);

/** 是否处于筛选态（空态文案据此变化） */
const hasFilter = computed(() =>
  Boolean(query.keyword || query.status || query.priority || query.owner_id || query.project_id || dueRange.value?.length),
);

/** 我的任务总条数（后端 `MyTasksRes.total`；未切到我的任务视图时为 0） */
const myTotalCount = ref(0);
/** 提示区展示的总数 */
const myTotal = computed(() => myTotalCount.value);

/** 列定义 */
const columns = [
  { colKey: 'task_no', title: '任务号', width: 132 },
  { colKey: 'title', title: '标题', width: 240, ellipsis: true },
  { colKey: 'status', title: '状态', width: 104 },
  { colKey: 'priority', title: '优先级', width: 96 },
  { colKey: 'progress', title: '进度', width: 140 },
  { colKey: 'owner_name', title: '负责人', width: 100 },
  { colKey: 'project_name', title: '项目', width: 140, ellipsis: true },
  { colKey: 'due_date', title: '截止日期', width: 130 },
  { colKey: 'source', title: '来源', width: 96 },
  { colKey: 'op', title: '操作', width: 150, fixed: 'right' as const },
];

/** 项目下拉选项（T7c 项目筛选；项目数量不大，一次取 200 条足够） */
const projectOptions = ref<Array<{ label: string; value: string }>>([]);
/** 项目下拉加载中 */
const projectLoading = ref(false);

/** 拉项目下拉（筛选用；失败不影响任务列表） */
async function loadProjectOptions(): Promise<void> {
  projectLoading.value = true;
  try {
    const data = await pageProjects({ page: 1, page_size: 200 });
    projectOptions.value = data.list.map((item) => ({ label: `${item.code} ${item.name}`, value: item.id }));
  } catch {
    projectOptions.value = [];
  } finally {
    projectLoading.value = false;
  }
}

/** 点项目名 → 跳项目页并直接打开该项目详情（`?id=`） */
function openProject(row: TaskCard): void {
  if (!row.project_id) {
    return;
  }
  void router.push({ path: '/project', query: { id: row.project_id } });
}

/** 来源文案 */
function sourceLabel(source: string): string {
  const map: Record<string, string> = { SELF: '自建', ASSIGNED: '被分派', TRANSFERRED: '转派' };
  return map[source] ?? source;
}

/** 是否逾期：有截止日、早于今天、且未完成/未取消（今天按本地 CST 计算） */
function isOverdue(row: TaskCard): boolean {
  if (!row.due_date || row.status === 'DONE' || row.status === 'CANCELLED') {
    return false;
  }
  return dayjs(row.due_date).startOf('day').isBefore(dayjs().startOf('day'));
}

/** 「我的任务」五组拍平成一张表（顺序 = 紧急度：逾期 → 今日 → 本周 → 以后 → 无期限） */
function flattenMyTasks(data: Awaited<ReturnType<typeof listMyTasks>>): TaskRow[] {
  const groups: Array<[string, TaskCard[]]> = [
    ['逾期', data.overdue],
    ['今日', data.today],
    ['本周', data.this_week],
    ['以后', data.later],
    ['无期限', data.no_due],
  ];
  return groups.flatMap(([label, list]) => list.map((item) => ({ ...item, bucketLabel: label })));
}

/** 路由（视图切换 / 跳甘特图都用它） */
const route = useRoute();
const router = useRouter();

/** 当前视图（`gantt` 只是切换开关的过渡值：选中即跳独立路由，本页不渲染甘特） */
const view = ref<'list' | 'board' | 'gantt'>(route.query.view === 'board' ? 'board' : 'list');
/** 看板实例（保存 / 删除后让它重拉） */
const boardRef = ref<InstanceType<typeof TaskBoard> | null>(null);
/** 看板总条数（工具条展示） */
const boardTotal = ref(0);

/** 工具条计数文案（列表 / 看板各用各的总数） */
const countText = computed(() => {
  const scope = myMode.value ? '我的任务' : '全部可见任务';
  const total = view.value === 'board' ? boardTotal.value : pagination.total;
  return `${scope} · 共 ${total} 条`;
});

/** 把视图写回 URL query（列表是默认值 → 删掉参数，URL 保持干净） */
function syncViewQuery(next: 'list' | 'board'): void {
  const current = typeof route.query.view === 'string' ? route.query.view : '';
  const target = next === 'board' ? 'board' : '';
  if (current === target) {
    return;
  }
  const nextQuery = { ...route.query };
  if (target) {
    nextQuery.view = target;
  } else {
    delete nextQuery.view;
  }
  void router.replace({ path: route.path, query: nextQuery });
}

/** 视图切换：甘特走独立路由，列表 / 看板在页内切 */
watch(view, (next) => {
  if (next === 'gantt') {
    void router.push('/task/gantt');
    return;
  }
  syncViewQuery(next);
  if (next === 'list') {
    // 看板里刚拖过状态 → 切回列表必须重拉，别拿旧数据糊弄
    void loadList();
  }
});

/** 浏览器前进 / 后退改了 query → 视图跟着回同步 */
watch(
  () => route.query.view,
  (value) => {
    const next = value === 'board' ? 'board' : 'list';
    if (view.value !== 'gantt' && view.value !== next) {
      view.value = next;
    }
  },
);

/** 刷新当前视图（列表重拉 / 看板重拉五列） */
function refreshCurrent(): void {
  if (view.value === 'board') {
    boardRef.value?.reload();
    return;
  }
  void loadList();
}

/** 看板每次加载后回报总条数 */
function handleBoardLoaded(total: number): void {
  boardTotal.value = total;
}

async function loadList(): Promise<void> {
  loading.value = true;
  errorMessage.value = '';
  try {
    if (myMode.value) {
      const data = await listMyTasks();
      rows.value = flattenMyTasks(data);
      myTotalCount.value = data.total;
      myCounts.value = data.counts;
      pagination.total = data.total;
      scopeTruncated.value = false;
      return;
    }
    const [dueFrom, dueTo] = dueRange.value ?? [];
    const data = await pageTasks({
      page: pagination.current,
      page_size: pagination.pageSize,
      keyword: query.keyword,
      status: query.status,
      priority: query.priority,
      owner_id: query.owner_id,
      // T7c：项目筛选只作为过滤条件，可见性仍由后端 T1 决定
      project_id: query.project_id,
      due_from: dueFrom || '',
      due_to: dueTo || '',
    });
    rows.value = data.list;
    pagination.total = data.total;
    scopeTruncated.value = data.member_scope_truncated;
    myTotalCount.value = 0;
    myCounts.value = null;
  } catch (error) {
    rows.value = [];
    pagination.total = 0;
    errorMessage.value = error instanceof ApiError ? error.message : '任务列表加载失败，请稍后重试';
  } finally {
    loading.value = false;
  }
}

/** 查询：回到第一页再拉 */
function handleSearch(): void {
  pagination.current = 1;
  void loadList();
}

/** 重置：清条件 + 回全部视图 */
function handleReset(): void {
  query.keyword = '';
  query.status = '';
  query.priority = '';
  query.owner_id = '';
  query.project_id = '';
  dueRange.value = [];
  myMode.value = false;
  pagination.current = 1;
  void loadList();
}

/** 翻页 */
function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  pagination.current = pageInfo.current;
  pagination.pageSize = pageInfo.pageSize;
  void loadList();
}

/** 切换「我的任务」：两种口径的数据源不同，切换即重拉 */
watch(myMode, () => {
  pagination.current = 1;
  void loadList();
});

/** 新建 */
function openCreate(): void {
  editingId.value = null;
  formVisible.value = true;
}

/** 编辑 */
function openEdit(row: TaskCard): void {
  editingId.value = row.id;
  formVisible.value = true;
}

/** 详情里点编辑：先关抽屉再开表单，避免两层遮罩叠在一起 */
function handleEditFromDetail(id: string): void {
  detailVisible.value = false;
  editingId.value = id;
  formVisible.value = true;
}

/** 查看详情 */
function openDetail(row: TaskCard): void {
  viewingId.value = row.id;
  detailVisible.value = true;
}

/** 保存成功：刷新当前视图（看板 / 列表各自的数据源） */
function handleSaved(): void {
  if (view.value === 'board') {
    boardRef.value?.reload();
    return;
  }
  void loadList();
}

/** 删除确认 */
function confirmRemove(row: TaskCard): void {
  const dialog = DialogPlugin.confirm({
    header: '删除任务',
    body: `确认删除「${row.title}」？删除后列表与详情都不再可见（软删除，可由运维恢复）。`,
    theme: 'warning',
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: async () => {
      try {
        const result = await deleteTask(row.id);
        if (result.deleted) {
          MessagePlugin.success('任务已删除');
        } else {
          MessagePlugin.warning('任务已被删除（重复操作）');
        }
        dialog.destroy();
        // 当前页删空后回退一页，避免停在空白页
        if (rows.value.length === 1 && pagination.current > 1) {
          pagination.current -= 1;
        }
        void loadList();
      } catch (error) {
        MessagePlugin.error(error instanceof ApiError ? error.message : '删除失败，请稍后重试');
      }
    },
  });
}

/** 详情抽屉里删除成功 → 刷新当前视图 */
function handleDeleted(): void {
  if (view.value === 'board') {
    boardRef.value?.reload();
    return;
  }
  void loadList();
}

onMounted(() => {
  // 从 URL 读项目筛选（项目页「打开任务」带 `?project_id=`，刷新也要保持）
  if (typeof route.query.project_id === 'string' && route.query.project_id) {
    query.project_id = route.query.project_id;
  }
  // 看板视图由 TaskBoard 自己在 onMounted 里拉五列，这里只负责列表
  void loadProjectOptions();
  if (view.value === 'list') {
    void loadList();
  }
});
</script>

<style scoped lang="less">
.task-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.filter-card :deep(.t-card__body) {
  padding-bottom: 4px;
}

.my-switch {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  cursor: pointer;

  &__label {
    font-size: 13px;
    color: var(--td-text-color-secondary);
  }
}

.filter-hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;

  &__count {
    font-size: 13px;
    color: var(--td-text-color-placeholder);
  }
}

.toolbar-card {
  :deep(.t-card__body) {
    padding: 10px 16px;
  }
}

.view-switch {
  margin-right: 4px;
}

.board-view {
  :deep(.t-card__body) {
    padding: 12px 16px;
  }
}

.table-alert {
  margin-bottom: 12px;
}

.cell-no {
  font-family: var(--app-font-mono, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace);
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.cell-title {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;

  &__text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
}

.cell-progress {
  display: flex;
  align-items: center;
  gap: 8px;

  :deep(.t-progress__bar) {
    flex: 1;
  }

  &__text {
    flex-shrink: 0;
    width: 34px;
    font-size: 12px;
    font-variant-numeric: tabular-nums;
    text-align: right;
    color: var(--td-text-color-secondary);
  }
}

.cell-due--overdue {
  font-weight: 600;
  color: var(--td-error-color);
}

.cell-project-empty {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.cell-bucket {
  display: block;
  font-size: 11px;
  color: var(--td-text-color-placeholder);
}

.empty-state {
  padding: 32px 0;
  text-align: center;

  &__icon {
    font-size: 40px;
    color: var(--td-text-color-placeholder);
  }

  &__title {
    margin: 12px 0 4px;
    font-size: 14px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__desc {
    margin: 0 0 12px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}
</style>

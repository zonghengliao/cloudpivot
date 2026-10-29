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
            <span class="filter-hint">
              我的任务按「逾期 / 今日 / 本周 / 以后 / 无期限」分组统计（CST 计算），共 {{ myTotal }} 条
            </span>
          </t-space>
        </t-col>
      </t-row>
    </t-card>

    <!-- 列表 -->
    <t-card :bordered="false" class="table-card">
      <div class="toolbar">
        <t-space>
          <t-button v-privilege="[PERM.taskCreate]" theme="primary" @click="openCreate">
            <template #icon><t-icon name="add" /></template>
            新建任务
          </t-button>
          <t-button variant="outline" :loading="loading" @click="loadList">
            <template #icon><t-icon name="refresh" /></template>
            刷新
          </t-button>
          <t-button variant="outline" @click="goGantt">
            <template #icon><t-icon name="chart" /></template>
            甘特图
          </t-button>
        </t-space>
        <span class="toolbar__count">{{ myMode ? '我的任务' : '全部可见任务' }} · 共 {{ pagination.total }} 条</span>
      </div>

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
 * # 状态流转（T2）未就绪
 *
 * 详情里的「开始 / 提交验收 / 完成 / 取消」按卡片要求**画出来但禁用**，
 * 接口位留在 `api/business/task-api.ts` 末尾的 TODO 注释里。
 */
import { computed, onMounted, reactive, ref, watch } from 'vue';
import { useRouter } from 'vue-router';
import { MessagePlugin, DialogPlugin } from 'tdesign-vue-next';
import dayjs from 'dayjs';
import StatusBadge from './components/StatusBadge.vue';
import PriorityTag from './components/PriorityTag.vue';
import UserSelect from './components/UserSelect.vue';
import TaskForm from './components/TaskForm.vue';
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
const query = reactive<Required<Pick<TaskPageQuery, 'keyword' | 'status' | 'priority' | 'owner_id'>>>({
  keyword: '',
  status: '',
  priority: '',
  owner_id: '',
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
const hasFilter = computed(
  () => Boolean(query.keyword || query.status || query.priority || query.owner_id || dueRange.value?.length),
);

/** 我的任务总条数（后端 `MyTasksRes.total`；未切到我的任务视图时为 0） */
const myTotalCount = ref(0);
/** 提示区展示的总数 */
const myTotal = computed(() => myTotalCount.value);

/** 列定义 */
const columns = [
  { colKey: 'task_no', title: '任务号', width: 132 },
  { colKey: 'title', title: '标题', width: 260, ellipsis: true },
  { colKey: 'status', title: '状态', width: 104 },
  { colKey: 'priority', title: '优先级', width: 96 },
  { colKey: 'progress', title: '进度', width: 140 },
  { colKey: 'owner_name', title: '负责人', width: 100 },
  { colKey: 'due_date', title: '截止日期', width: 130 },
  { colKey: 'source', title: '来源', width: 96 },
  { colKey: 'op', title: '操作', width: 150, fixed: 'right' as const },
];

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

/** 拉列表 */
/** 路由（跳甘特图 / 回列表都用它） */
const router = useRouter();

/** 跳只读甘特图（T4；拖拽改期属 T7） */
function goGantt(): void {
  void router.push('/task/gantt');
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

/** 保存成功：刷新列表并提示 */
function handleSaved(): void {
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

/** 详情抽屉里删除成功 → 刷新列表 */
function handleDeleted(): void {
  void loadList();
}

onMounted(() => {
  void loadList();
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
  margin-bottom: 12px;

  &__count {
    font-size: 13px;
    color: var(--td-text-color-placeholder);
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

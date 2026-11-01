<template>
  <div class="task-page">
    <!-- 筛选区 -->
    <t-card :bordered="false" class="filter-card">
      <!-- 三行统一样式（与甘特图一致）：控件等宽 240px -->
      <div class="filter-line">
        <t-input v-model="query.keyword" class="filter-ctl filter-ctl--wide" placeholder="任务标题 / 描述关键词" clearable @enter="handleSearch">
          <template #prefix-icon><t-icon name="search" /></template>
        </t-input>
        <!-- T9-B：状态筛选从这一行移出，改成表格上方的勾选标签（可多选） -->
        <t-select v-model="query.priority" class="filter-ctl" placeholder="优先级" clearable :options="priorityOpts" />
        <UserSelect v-model="query.owner_id" class="filter-ctl" placeholder="负责人" :disabled="myMode" />
        <t-select
          v-model="query.project_id"
          class="filter-ctl"
          :options="projectOptions"
          :loading="projectLoading"
          placeholder="所属项目"
          clearable
          filterable
          :disabled="myMode"
        />
        <!-- P-FLOW-1C：「来源」筛选。`FLOW` = 流程节点生成的任务（P-FLOW-1A 扩的 source 值） -->
        <t-select
          v-model="query.source"
          class="filter-ctl"
          :options="sourceOpts"
          :disabled="myMode"
          placeholder="来源"
          data-testid="task-source-filter"
        />
      </div>
      <div class="filter-line">
        <t-date-range-picker v-model="dueRange" class="filter-ctl filter-ctl--wide" placeholder="截止日期范围" clearable format="YYYY-MM-DD" />
      </div>
      <div class="filter-line filter-line--actions">
        <t-button theme="primary" @click="handleSearch">
          <template #icon><t-icon name="search" /></template>
          查询
        </t-button>
        <t-button variant="outline" @click="handleReset">重置</t-button>
      </div>
      <div class="filter-line filter-line--mine">
        <label class="my-switch">
          <t-switch v-model="myMode" size="small" />
          <span class="my-switch__label">只看我的任务</span>
        </label>
        <span v-if="view === 'list'" class="filter-hint">
          我的任务按「逾期 / 今日 / 本周 / 以后 / 无期限」分组统计（CST 计算），共 {{ myTotal }} 条
        </span>
      </div>
      <!-- O4 列表记忆：四个视图共用这一套筛选，恢复后必须让用户看得见 -->
      <ListMemoryBar
        :restored="listMemoryRestored"
        :text="listMemoryText"
        testid="task-memory"
        @clear="handleClearListMemory"
      />
    </t-card>

    <!-- 工具条：三视图切换 + 主操作（列表 / 看板共用同一份筛选条件） -->
    <t-card :bordered="false" class="toolbar-card">
      <div class="toolbar">
        <t-space align="center">
          <t-tabs v-model="view" class="view-switch" :space-evenly="false">
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
          <t-button variant="outline" size="small" :loading="loading" @click="refreshCurrent">
            <template #icon><t-icon name="refresh" /></template>
            刷新
          </t-button>
        </t-space>
        <span class="toolbar__count">{{ countText }}</span>
      </div>
    </t-card>

    <!-- 状态筛选：勾选标签、可多选，紧贴表格上方（老大 2026-10-30 要求，T9-B）
         只在列表视图出现 —— 看板按状态分列显示全部，勾选在这里没有意义 -->
    <div v-if="view === 'list'" class="status-bar">
      <span class="status-bar__label">状态</span>
      <t-check-tag
        v-for="opt in statusOpts"
        :key="opt.value"
        :checked="query.statuses.includes(opt.value)"
        class="status-bar__tag"
        @change="toggleStatus(opt.value)"
      >
        {{ opt.label }}
      </t-check-tag>
      <t-button
        v-if="query.statuses.length > 0"
        variant="text"
        size="small"
        class="status-bar__clear"
        @click="clearStatuses"
      >
        清空（{{ query.statuses.length }}）
      </t-button>
    </div>

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

      <!-- 树形表格要用 t-enhanced-table（`t-table` 是 PrimaryTable，不认 tree） -->
      <t-enhanced-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="tablePagination"
        :tree="treeConfig"
        :expanded-tree-nodes="expandedKeys"
        :row-class-name="rowClassName"
        :hover="true"
        stripe
        @page-change="handlePageChange"
        @expanded-tree-nodes-change="handleExpandedKeysChange"
        @tree-expand-change="handleTreeExpandChange"
      >
        <template #task_no="{ row }">
          <span class="cell-no">{{ row.task_no }}</span>
        </template>

        <template #title="{ row }">
          <div class="cell-title">
            <!-- F2：新任务标记（A 口径：TODO ∧ 负责人是我 ∧ 7 天内创建，后端 is_new 下发） -->
            <NewDot
              v-if="row.is_new"
              class="cell-title__new"
              variant="badge"
              tone="danger"
              title="新任务：待开始、负责人是我、7 天内创建"
            />
            <!-- P-FLOW-1C：「流程」徽标。hover 才去查流程名 / 节点名（列表 DTO 不为此多 JOIN） -->
            <t-tooltip v-if="isFlowRow(row)" placement="top" :show-arrow="true">
              <template #content>
                <div class="flow-tip" data-testid="task-flow-tip">{{ flowTipText(row) }}</div>
              </template>
              <t-tag
                size="small"
                theme="primary"
                variant="light"
                class="cell-title__flow"
                data-testid="task-flow-badge"
                @mouseenter="prefetchFlow(row)"
              >
                流程
              </t-tag>
            </t-tooltip>
            <t-link
              theme="primary"
              class="cell-title__text"
              :class="{ 'cell-title__text--new': row.is_new }"
              @click="openDetail(row)"
            >{{ row.title }}</t-link>
            <!-- T8-B：子任务数从标题列搬到「进度」列（小进度条 + N/M 分数），标题列只留标题 -->
            <t-tag v-if="row.need_review" size="small" variant="outline" theme="default">需验收</t-tag>
          </div>
        </template>

        <template #status="{ row }">
          <StatusBadge domain="task" :status="row.status" size="small" />
        </template>

        <template #priority="{ row }">
          <PriorityTag :priority="row.priority" />
        </template>

        <template #progress="{ row }">
          <!-- T8-B ③：父任务行显示「子任务完成度」（小进度条 + N/M），非父任务显示自己的进度 -->
          <SubProgressBar
            v-if="isParentRow(row)"
            :done="row.child_done_count"
            :total="row.child_count"
          />
          <div v-else class="cell-progress">
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
      </t-enhanced-table>
    </t-card>

    <!-- 看板视图（交互见 components/TaskBoard.vue：原生 HTML5 拖拽改状态） -->
    <t-card v-else-if="view === 'board'" :bordered="false" class="board-view">
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

    <!-- 燃尽图（E11，只读；数据后端算好，前端只画） -->
    <t-card v-else-if="view === 'burndown'" :bordered="false" class="burndown-view">
      <TaskBurndown :project-id="query.project_id" />
    </t-card>

    <!-- 新建 / 编辑 -->
    <TaskForm v-model:visible="formVisible" :task-id="editingId" @saved="handleSaved" />

    <!-- 详情抽屉 -->
    <TaskDetail
      v-model:visible="detailVisible"
      :task-id="viewingId"
      @edit="handleEditFromDetail"
      @deleted="handleDeleted"
      @changed="handleSaved"
    />
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
import type { TableRowData, TableTreeExpandChangeContext } from 'tdesign-vue-next';
import dayjs from 'dayjs';
import StatusBadge from './components/StatusBadge.vue';
import PriorityTag from './components/PriorityTag.vue';
import SubProgressBar from './components/SubProgressBar.vue';
import UserSelect from './components/UserSelect.vue';
import TaskForm from './components/TaskForm.vue';
import TaskBurndown from './components/TaskBurndown.vue';
import TaskBoard from './components/TaskBoard.vue';
import TaskDetail from './detail.vue';
import NewDot from '@/components/NewDot.vue';
import ListMemoryBar from '@/components/ListMemoryBar.vue';
import { useListFilterMemory } from '@/composables/useListFilterMemory';
import { isParentRow, rowClassNameOf, treeConfigOf } from './components/tree-layout';
import { clearStatusList, hasStatusFilter, statusesParam, toggleStatusIn } from './components/status-filter';
import { ApiError } from '@/api/request';
import {
  deleteTask,
  getTaskFlow,
  listMyTasks,
  listSubtasks,
  pageTasks,
  type MyTaskCounts,
  type TaskCard,
  type TaskFlowDetail,
  type TaskPageQuery,
} from '@/api/business/task-api';
import { pageProjects } from '@/api/business/project-api';
import { PERM } from '@/constants/permissions';
import { priorityOptions, statusOptions } from '@/constants/status-map';

/**
 * 列表行：任务卡 + 树形表格辅助字段。
 *
 * `children` 是 TDesign 树形表格认识的字段：
 * - `true` = 有子任务但**还没取**（箭头照常渲染，内部 toggle 会直接 return，由本页自己懒加载）；
 * - `TaskRow[]` = 已加载的子行；
 * - 不存在 = 叶子（无箭头）。
 *
 * `_isSub` 标记子行（状态徽标缩小、不加粗），`bucketLabel` 只在我的任务视图有值。
 */
type TaskRow = TaskCard & {
  bucketLabel?: string;
  _isSub?: boolean;
  children?: TaskRow[];
};

/**
 * 「有子任务但还没取」的占位值。
 *
 * TDesign 树形表格的运行时约定：`children === true` 时照常渲染展开箭头，但内部的
 * `toggleExpandData` 直接 return（不凭空展开）—— 正是懒加载要的语义。
 * 它的 TS 类型却把 `children` 限成 `TableRowData[]`，所以这里做一次窄化断言，
 * 并且**只通过 `isLazyChildren` / `setLazyChildren` 读写**，别在别处直接比较。
 */
const LAZY_CHILDREN = true as unknown as TaskRow[];

/** 该行是否处于「有子任务但未加载」占位态 */
function isLazyChildren(row: TaskRow): boolean {
  return (row.children as unknown) === true;
}

/** 把行打回占位态（懒加载失败时还原，允许再点一次） */
function setLazyChildren(row: TaskRow): void {
  row.children = LAZY_CHILDREN;
}

/** 状态 / 优先级筛选选项（文案与徽标同源，来自 status-map） */
const statusOpts = statusOptions('task');
const priorityOpts = priorityOptions();

/**
 * 「来源」筛选选项（P-FLOW-1C）。
 *
 * 用哨兵值 `'all'` 而不是空串：TDesign 的 `t-select` 把空串当「未选」，
 * 选中「全部」后显示的是 placeholder（本仓踩过，见 `cloudpivot-frontend-verify` 的笔记）。
 * `buildSourceParam()` 负责把 `'all'` 变成「整键缺席」。
 */
const sourceOpts = [
  { label: '全部来源', value: 'all' },
  { label: '流程', value: 'FLOW' },
  { label: '自建', value: 'SELF' },
  { label: '被分派', value: 'ASSIGNED' },
  { label: '转派', value: 'TRANSFERRED' },
  { label: '周期', value: 'RECURRING' },
];

/** 来源过滤参数：`'all'` → 不带这个键（`asParams` 会丢掉空串） */
function buildSourceParam(): string {
  return query.source === 'all' ? '' : query.source;
}

/** 表格数据 */
const rows = ref<TaskRow[]>([]);
/** 正在拉子任务的父任务 id（防连点重复请求） */
const subtaskLoading = new Set<string>();
/**
 * 已展开的树节点（受控）。
 *
 * 为什么必须自己拿着：子任务懒加载完要重建 `rows` 让表格重算层级（不重建的话
 * 后插进来的子行 `level` 是 undefined → 不缩进），而重建会走 `resetData`，
 * 只有受控展开态才能让「刚展开的那一行」在重建后仍然展开。
 */
const expandedKeys = ref<Array<string | number>>([]);
/** 加载中 */
const loading = ref(false);
/** 错误文案（非空即渲染错误态 + 重试） */
const errorMessage = ref('');
/** 「我参与」id 数组被后端截断（本视图只含数据范围内任务） */
const scopeTruncated = ref(false);

/**
 * 筛选条件。
 *
 * `statuses` 是**数组**（T9-B：勾选标签多选），不在 `TaskPageQuery` 里 ——
 * 单值 `status` 已随下拉一起下线，多值参数由 {@link statusesParam} 序列化。
 */
type QueryState = Required<Pick<TaskPageQuery, 'keyword' | 'priority' | 'owner_id' | 'project_id'>> & {
  statuses: string[];
  /** 来源过滤（`'all'` = 不筛；`'FLOW'` = 只看流程生成的任务） */
  source: string;
};
const query = reactive<QueryState>({
  keyword: '',
  statuses: [],
  priority: '',
  owner_id: '',
  // 项目筛选：初值留空，`onMounted` 里从 URL query 读（项目页「查看项目下任务」会带过来）
  project_id: '',
  source: 'all',
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
  Boolean(
    query.keyword ||
      hasStatusFilter(query.statuses) ||
      query.priority ||
      query.owner_id ||
      query.project_id ||
      (query.source && query.source !== 'all') ||
      dueRange.value?.length,
  ),
);

/** 我的任务总条数（后端 `MyTasksRes.total`；未切到我的任务视图时为 0） */
const myTotalCount = ref(0);
/** 提示区展示的总数 */
const myTotal = computed(() => myTotalCount.value);

/** 列定义 */
const columns = [
  // 树形箭头挂在任务号列（老大 2026-10-30），缩进 24px/层最深吃掉 168px，
  // 所以列宽从 132 加到 170 —— 否则 17 字符的号（TSK-202610-000757）会被截断
  { colKey: 'task_no', title: '任务号', width: 170 },
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

/**
 * 树形表格配置（T8-B ①）：箭头挂在**标题列**（`colKey: 'title'`），缩进 24px。
 *
 * E6b 写死 `treeNodeColumnIndex: 0` 把箭头挂在「任务号」列 —— 用户看标题不看任务号，
 * 展开箭头离标题隔着整列，点起来也别扭。这里按 colKey 反查下标（见 `tree-layout.ts`），
 * 以后列顺序调整（比如在任务号前插一列）也不用再改这个数字。
 */
const treeConfig = treeConfigOf(columns.map((col) => col.colKey as string));

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
  const map: Record<string, string> = {
    SELF: '自建',
    ASSIGNED: '被分派',
    TRANSFERRED: '转派',
    RECURRING: '周期',
    // P-FLOW-1A 新增：流程节点生成的任务（`source='FLOW'` ⇔ `flow_instance_id` 非空）
    FLOW: '流程',
  };
  return map[source] ?? source;
}

// ==================== 流程徽标（P-FLOW-1C） ====================

/** 该行是否流程生成的任务（后端 `flow` 段非空 ⇔ `source='FLOW'`） */
function isFlowRow(row: TaskCard): boolean {
  return Boolean(row.flow) || row.source === 'FLOW';
}

/**
 * 已查到的流程上下文（按任务 id 缓存）。
 *
 * 列表 DTO 只给 `instance_id / node_key / round`（**不为此多 JOIN**），
 * 所以「流程名 · 节点名」要在 hover 时按需查 `GET /business/task/{id}/flow`。
 * 缓存键是任务 id：同一行反复 hover 只打一次。
 */
const flowInfo = ref<Record<string, TaskFlowDetail | null>>({});
/** 正在查的任务 id（防连点重复请求） */
const flowLoading = new Set<string>();

/** hover 时预取流程上下文（失败就标记 null，不再重试） */
function prefetchFlow(row: TaskCard): void {
  const id = row.id;
  if (id in flowInfo.value || flowLoading.has(id)) {
    return;
  }
  flowLoading.add(id);
  void getTaskFlow(id)
    .then((response) => {
      flowInfo.value = { ...flowInfo.value, [id]: response.flow ?? null };
    })
    .catch(() => {
      flowInfo.value = { ...flowInfo.value, [id]: null };
    })
    .finally(() => {
      flowLoading.delete(id);
    });
}

/** 徽标 hover 的正文：查到了给「流程名 › 节点名（第 N 轮）」，没查到先给节点 key */
function flowTipText(row: TaskCard): string {
  const flow = flowInfo.value[row.id];
  if (flow) {
    return `${flow.flow_name} › ${flow.node_name}（第 ${flow.round} 轮）`;
  }
  const node = row.flow?.node_key ?? '';
  const round = row.flow?.round ?? 1;
  return flowLoading.has(row.id)
    ? `流程任务 · ${node}（第 ${round} 轮）· 正在加载流程名…`
    : `流程任务 · ${node}（第 ${round} 轮）`;
}

/** 是否逾期：有截止日、早于今天、且未完成/未取消（今天按本地 CST 计算） */
function isOverdue(row: TaskCard): boolean {
  if (!row.due_date || row.status === 'DONE' || row.status === 'CANCELLED') {
    return false;
  }
  return dayjs(row.due_date).startOf('day').isBefore(dayjs().startOf('day'));
}

/** 把任务卡加工成树形行：有子任务的先给 `children: true` 占位（渲染箭头，等点击才取数） */
function toTreeRow(card: TaskCard, extra?: Partial<TaskRow>): TaskRow {
  return { ...card, children: card.child_count > 0 ? LAZY_CHILDREN : undefined, ...extra };
}

/** 子任务行：标记 `_isSub`（徽标缩小、不加粗），自己若也有子任务同样可再展开（支持三级） */
function toSubRow(card: TaskCard): TaskRow {
  return { ...card, _isSub: true, children: card.child_count > 0 ? LAZY_CHILDREN : undefined };
}

/**
 * 顶层行筛选：**只把「无父任务」或「父任务不在本页结果集里」的行放顶层**。
 *
 * - 子任务即使出现在分页结果里也不上浮（跟随父任务，展开时由 `GET /subtasks` 带出），
 *   否则同一条子任务会既在顶层、又在父任务下面出现两份；
 * - 「我的任务」按人筛选时多数父任务不在结果里 → 子任务自然成为顶层（卡面边界要求）。
 */
function buildTreeRows(list: TaskCard[]): TaskRow[] {
  const ids = new Set(list.map((item) => item.id));
  return list
    .filter((item) => !item.parent_id || !ids.has(item.parent_id))
    .map((item) => toTreeRow(item));
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
  // 我的任务视图不按父子嵌套（父任务多半不在结果里），有子任务的行仍可展开
  return groups.flatMap(([label, list]) => list.map((item) => toTreeRow(item, { bucketLabel: label })));
}

/**
 * 行样式（T8-B ②④）：父任务 `task-row--parent`（4px 主色左竖条 + 浅底 + 加粗）、
 * 子任务 `task-row--child`（连接线 + 更浅底）、普通行不加类。
 *
 * 缩进由 `t-enhanced-table` 的 tree（`indent: 24`）自动算，**不在这里手写 margin-left**；
 * 判断逻辑抽到 `tree-layout.ts` 里做了单测（父/子/普通三态 + 孙任务优先按子行渲染）。
 */
function rowClassName(context: { row: TaskRow }): string {
  return rowClassNameOf(context.row);
}

/**
 * 展开箭头点击 → 懒加载子任务。
 *
 * TDesign 树形表格用 `children === true` 表示「有子节点但还没取」：箭头照常渲染，
 * 但内部 `toggleExpandData` 遇到 `children === true` 直接 return（不会凭空展开）。
 * 所以这里自己取数 → 把占位换成真数组 → **重建 rows** 让表格重算层级（`level`/缩进
 * 只在 `initialTreeDataMap` 里算），受控的 `expandedKeys` 保证重建后这一行仍是展开态。
 * 已加载（数组）的行折叠 / 再展开全交给表格自己处理，不再发请求。
 */
async function handleTreeExpandChange(
  context: TableTreeExpandChangeContext<TableRowData>,
): Promise<void> {
  const row = context.row as TaskRow;
  if (!row || !isLazyChildren(row) || subtaskLoading.has(row.id)) {
    return;
  }
  subtaskLoading.add(row.id);
  try {
    const data = await listSubtasks(row.id);
    row.children = data.list.map(toSubRow);
    // 重建数组：触发 resetData → 子行拿到 level=1（不重建则不缩进）
    rows.value = [...rows.value];
  } catch (error) {
    // 还原占位，允许用户再点一次
    setLazyChildren(row);
    MessagePlugin.error(error instanceof ApiError ? error.message : '子任务加载失败，请稍后重试');
  } finally {
    subtaskLoading.delete(row.id);
  }
}

/** 表格受控展开态同步（折叠 / 展开都由表格判定，这里只是镜像到本地 ref） */
function handleExpandedKeysChange(keys: Array<string | number>): void {
  expandedKeys.value = keys;
}

/** 路由（视图切换 / 跳甘特图都用它） */
const route = useRoute();
const router = useRouter();

/** 当前视图（`gantt` 只是切换开关的过渡值：选中即跳独立路由，本页不渲染甘特） */
// 初始视图优先级：URL query > 路由 meta.defaultView（V0130 菜单目录化）> 列表
const _metaView = (route.meta as any)?.defaultView as 'list' | 'board' | undefined;
const _queryView = typeof route.query.view === 'string' ? route.query.view : '';
const view = ref<'list' | 'board' | 'gantt' | 'burndown'>(
  _queryView === 'gantt' ? 'gantt'
    : _queryView === 'burndown' ? 'burndown'
    : _queryView === 'board' ? 'board'
    : _queryView === 'list' ? 'list'
    : _metaView === 'board' ? 'board'
    : 'list',
);

// V0130 修复：`/task/list` 与 `/task/board` 共用本组件，Vue Router 会复用实例，
// setup() 不会重跑 ⇒ 只靠初始化时读 meta 会导致「菜单互切无反应」。这里监听路由。
watch(
  () => route.meta?.defaultView as 'list' | 'board' | undefined,
  (v) => {
    if (v === 'list' || v === 'board') {
      view.value = v;
      void refreshCurrent();
    }
  },
);
/** 看板实例（保存 / 删除后让它重拉） */
const boardRef = ref<InstanceType<typeof TaskBoard> | null>(null);
/** 看板总条数（工具条展示） */
const boardTotal = ref(0);

/** 工具条计数文案（列表 / 看板各用各的总数） */
const countText = computed(() => {
  const scope = myMode.value ? '我的任务' : '全部可见任务';
  if (view.value === 'burndown') {
    return `${scope} · 燃尽图`;
  }
  const total = view.value === 'board' ? boardTotal.value : pagination.total;
  return `${scope} · 共 ${total} 条`;
});

/** 把视图写回 URL query（列表是默认值 → 删掉参数，URL 保持干净） */
function syncViewQuery(next: 'list' | 'board' | 'burndown'): void {
  const current = typeof route.query.view === 'string' ? route.query.view : '';
  const target = next === 'board' ? 'board' : next === 'burndown' ? 'burndown' : '';
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
    // E11：`burndown` 也是页内视图 —— 漏掉它会把「点了燃尽图 Tab」立刻改回 list
    // （`syncViewQuery` 写入 ?view=burndown → 本 watcher 把它当成未知值回落 list，
    //   现象是「点 Tab 没反应」）。
    const next = value === 'board' ? 'board' : value === 'burndown' ? 'burndown' : 'list';
    if (view.value !== 'gantt' && view.value !== next) {
      view.value = next;
    }
  },
);

// ==================== 记忆（O4 列表） ====================

/** 分页器的 page / pageSize 包成可写 computed（分页器是 reactive 对象，记忆层要 Ref） */
const pageRef = computed({
  get: () => pagination.current,
  set: (value: number) => {
    pagination.current = value;
  },
});
const pageSizeRef = computed({
  get: () => pagination.pageSize,
  set: (value: number) => {
    pagination.pageSize = value;
  },
});
/** 视图包成 `Ref<string>`（记忆层只认字符串） */
const viewRef = computed({
  get: () => view.value as string,
  set: (value: string) => {
    view.value = value as 'list' | 'board' | 'gantt' | 'burndown';
  },
});

/**
 * 截止日期范围不在 `query` 里（独立 ref）→ 当作「额外记忆字段」交给记忆层：
 * 键名 `due_range` 与 `query` 里的字段不撞名，恢复时按同名键回写 `dueRange`。
 */
const TASK_MEMORY_EXTRA = {
  due_range: {
    read: () => dueRange.value ?? [],
    write: (value: unknown) => {
      dueRange.value = Array.isArray(value) ? value.map((item) => String(item)).filter(Boolean) : [];
    },
  },
};

/**
 * 列表记忆（O4）：四个视图共用同一套筛选条件，所以只记一份（关键词 / 状态多选 /
 * 优先级 / 负责人 / 项目 / 截止日范围 + 页码 + 每页条数）。
 *
 * Tab **只恢复 `burndown`**：list / board 各有独立菜单路由，初始视图由
 * `route.meta.defaultView` 决定；记忆去抢会把 V0130 修过的「点菜单切视图」重新搞坏。
 */
const {
  restored: listMemoryRestored,
  restoredText: listMemoryText,
  forget: forgetListMemory,
} = useListFilterMemory<QueryState>({
  listName: 'business-task',
  filter: query,
  page: pageRef,
  pageSize: pageSizeRef,
  tab: viewRef,
  defaultTab: 'list',
  allowedTabs: ['burndown'],
  extra: TASK_MEMORY_EXTRA,
});

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
  // 换数据源 / 翻页一律收起树（旧展开态对应的是旧行的 children 数组）
  expandedKeys.value = [];
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
    // T9-B：状态多选 → `statuses=TODO,DOING`（空 = 不传，后端即不筛）。
    // `TaskPageQuery` 是 api 层的契约文件（本卡不动），多值字段先用局部交叉类型收口；
    // 等 T9-A 的多值接口落地后把它并回 `TaskPageQuery` 即可，调用点无需再改。
    const data = await pageTasks({
      page: pagination.current,
      page_size: pagination.pageSize,
      keyword: query.keyword,
      statuses: statusesParam(query.statuses),
      priority: query.priority,
      owner_id: query.owner_id,
      // T7c：项目筛选只作为过滤条件，可见性仍由后端 T1 决定
      project_id: query.project_id,
      due_from: dueFrom || '',
      due_to: dueTo || '',
      // P-FLOW-1C：来源过滤（`'all'` → 空串 → `asParams` 整键丢掉，后端即不筛）
      source: buildSourceParam(),
      // T8-C：列表视图默认只取**顶层任务**（树形 + 分页唯一正确的组合）。
      // 子任务靠 TDesign 展开箭头逐层懒加载（listSubtasks）；看板 / 甘特 / 燃尽图
      // 走各自的接口，不传这个参数 —— 它们不是树形，需要全部任务。
      root_only: true,
    } as TaskPageQuery & { statuses?: string });
    rows.value = buildTreeRows(data.list);
    pagination.total = data.total;
    // 恢复的页码可能超出（数据变少 / 筛选变窄）：夹回最后一页，别停在空页上
    const maxPage = Math.max(1, Math.ceil(pagination.total / pagination.pageSize));
    pagination.current = Math.min(Math.max(1, pagination.current), maxPage);
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

/**
 * 勾选 / 取消一个状态（多选）。
 *
 * 勾选即查询 —— 老大要的是「直接筛选」，不该再点一次「查询」按钮
 * （关键词 / 优先级要保持「打完字再点查询」的节奏，所以它们不走这里）。
 */
function toggleStatus(value: string): void {
  query.statuses = toggleStatusIn(query.statuses, value);
  handleSearch();
}

/** 清空所有状态勾选（同样立即查询，回到全量） */
function clearStatuses(): void {
  query.statuses = clearStatusList();
  handleSearch();
}

/** 查询：回到第一页再拉 */
function handleSearch(): void {
  pagination.current = 1;
  void loadList();
}

/** 重置：清条件 + 回全部视图 */
function handleReset(): void {
  query.keyword = '';
  query.statuses = clearStatusList();
  query.priority = '';
  query.owner_id = '';
  query.project_id = '';
  query.source = 'all';
  dueRange.value = [];
  myMode.value = false;
  pagination.current = 1;
  void loadList();
}

/**
 * 列表记忆 · 「清空筛选」（提示条上的出口）。
 *
 * 复用页面自己的重置口径（含「只看我的任务」开关），再删掉 sessionStorage 里的记忆
 * —— 只清界面不清记忆的话，下次进页面又会恢复回来。
 */
function handleClearListMemory(): void {
  handleReset();
  forgetListMemory();
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
  // 通知「查看详情」带 ?id={task_id} → 自动打开详情抽屉（V0130 之后的兼容口径）
  const queryId = typeof route.query.id === 'string' ? route.query.id : '';
  if (queryId) {
    viewingId.value = queryId;
    detailVisible.value = true;
  }
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

/* 状态勾选条：紧贴表格上方，一行铺开（T9-B） */
.status-bar {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
  padding: 8px 16px;
  background: var(--td-bg-color-container);
  border-radius: var(--td-radius-medium);

  &__label {
    color: var(--td-text-color-secondary);
    font-size: 13px;
    margin-right: 4px;
  }

  /* t-check-tag 自带样式，这里只把默认外边距抹平（间距统一走 flex gap） */
  &__tag {
    margin: 0;
  }

  &__clear {
    margin-left: auto;
  }
}

/* 勾选条已经紧贴表格上方：抹掉表格卡片自带的 16px 上外边距，
   只保留页面级 12px 行距（否则勾选条与表格之间空出 28px，看起来是一块无主空白） */
.status-bar + .table-card {
  margin-top: 0;
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

  /* P-FLOW-1C：「流程」徽标。`flex: none` 保证被长标题挤的时候不缩成一条线 */
  &__flow {
    flex: none;
    cursor: default;
  }

  &__text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
}

/* ===================== T8-B：树形层级视觉（只用主题 token，0 裸 hex） ===================== */

/**
 * 标题列同时是树形列（T8-B ①）—— 必须把树形容器改回 flex 一行排。
 *
 * 因为标题列声明了 `ellipsis: true`，TDesign 会给树形容器加 `--inline`（`display: inline`），
 * 于是「行内箭头 + 块级 .cell-title」被拆成两行：实测箭头与标题垂直差 23px，行也变高。
 * 非叶子行容器是 `.t-table__tree-col`、叶子行是 `.t-table__tree-leaf-node`（两者都带 `--inline`），
 * 所以两个都要覆盖。截断不受影响：`.cell-title__text` 自己的 `text-overflow: ellipsis` 才是实际生效的那一层。
 */
:deep(.t-table__tree-col),
:deep(.t-table__tree-leaf-node) {
  display: flex;
  align-items: center;
  min-width: 0;
}

/**
 * 父任务行：4px 主色左竖条 + 品牌浅底 + 标题加粗。
 *
 * 底色打在 `td` 上（不是 `tr`）：TDesign 的 stripe / hover 都是给 `tr` 上色的，
 * 打在 `tr` 上会被「奇数行浅底」盖住，只有 `td` 才稳。代价是 hover 也被盖住，
 * 所以下面单独补一条 `:hover > td` 的深一档底色，悬停反馈不丢。
 * 竖条吃掉 4px 内边距，所以把 padding-left 减 4px —— 父行和普通行的任务号仍然对齐。
 */
:deep(.task-row--parent > td) {
  /* 父行底色：品牌色 6% 混白（老大定稿：4% 太淡，6% 刚好）。
   * 原来用 --td-brand-color-light（≈10% 品牌色），视觉上偏重、和子行的灰底拉不开层次。
   * 第一行是老浏览器回退（不支持 color-mix 时仍是品牌浅色，不会掉成透明）。 */
  background-color: var(--td-brand-color-light);
  background-color: color-mix(in srgb, var(--td-brand-color) 6%, var(--td-bg-color-container));
}

:deep(.task-row--parent > td:first-child) {
  border-left: 4px solid var(--td-brand-color);
  padding-left: calc(var(--td-comp-paddingLR-l) - 4px);
}

:deep(.task-row--parent:hover > td) {
  background-color: var(--td-brand-color-light-hover);
  background-color: color-mix(in srgb, var(--td-brand-color) 12%, var(--td-bg-color-container));
}

:deep(.task-row--parent .cell-title__text) {
  font-weight: 600;
}

/**
 * 子任务行：比父行更浅一档的底（灰底 vs 父行的品牌浅底，层级一眼分得出）。
 * 缩进由 `t-enhanced-table` 的 tree（`indent: 24`）自动算，这里不写 margin-left。
 */
:deep(.task-row--child > td) {
  background-color: var(--td-bg-color-secondarycontainer);
}

:deep(.task-row--child:hover > td) {
  background-color: var(--td-bg-color-secondarycontainer-hover);
}

/**
 * 子任务连接线（└─）：画在树形箭头 `.t-table__tree-op-icon` 的**前面**。
 *
 * 为什么不按卡面写在 `.cell-title::before`：箭头是表格在标题单元格里自己渲染的，
 * 插在 `.cell-title` 里会跑成「▼└─ 标题」，连接线跑到箭头后面去了。
 * 挂在图标前面才是「└─▼ 标题」，而且**跟着缩进一起移动**（图标本身带 level*indent 的
 * padding-left），8 层都不会错位。叶子行（已加载完的子任务）图标是空的，
 * 这个 ::before 同样生效，只是后面没有箭头。
 */
:deep(.task-row--child .t-table__tree-op-icon::before) {
  content: '';
  display: block;
  flex: none;
  width: 12px;
  height: 12px;
  margin-right: var(--td-comp-margin-xxs);
  border-left: 1px solid var(--td-component-stroke);
  border-bottom: 1px solid var(--td-component-stroke);
  border-bottom-left-radius: var(--td-radius-default);
}

/* F2：新任务标题加粗。写成两级选择器并放在父行规则之后——同权重下后写的这条
   才压得住父行的 600（新任务若也是父任务，仍要显式声明，避免以后调父行字重时被带走）。 */
.cell-title .cell-title__text--new {
  font-weight: 600;
}

/* F2：NEW 角标不参与标题的省略号收窄 */
.cell-title__new {
  flex: none;
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

/* 查询框三行布局（与甘特图一致） */
.filter-line {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 12px;
  flex-wrap: wrap;

  &:last-of-type {
    margin-bottom: 0;
  }
}

.filter-ctl {
  width: 200px;
  flex: 0 0 200px;
}

.filter-ctl--wide {
  width: 240px;
  flex: 0 0 240px;
}

/* 第3行（查询/重置）按钮左对齐 */
.filter-line--actions {
  justify-content: flex-start;
}

/* 第4行：只看我的任务 + 提示文字（左对齐） */
.filter-line--mine {
  justify-content: flex-start;
  gap: 12px;
}
</style>

<style lang="less">
/*
 * 「流程」徽标的 tooltip 正文（P-FLOW-1C）。
 *
 * t-tooltip 的弹层 teleport 到 body，本组件的 `scoped` 属性选择器命中不了它，
 * 所以这一条**故意不 scoped**（同类情况见本仓 `flow/` 的属性面板弹层）。
 */
.flow-tip {
  max-width: 260px;
  font-size: 12px;
  line-height: 1.6;
  word-break: break-all;
}
</style>

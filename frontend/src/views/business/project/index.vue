<template>
  <div class="project-page">
    <!-- 筛选 / 分页 / 行操作交给 ListPage（P3-12） -->
    <ListPage
      ref="listRef"
      title="项目分组"
      testid="project"
      table-testid="project-table"
      meta-testid="project-meta"
      btn-prefix="project-btn"
      meta-unit="个"
      :columns="columns"
      :filters="filters"
      :filter-state="query"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      :page-size="PROJECT_PAGE_SIZE"
      :show-page-size="true"
      @loaded="onLoaded"
    >
      <!-- 页头提示：项目只是任务的逻辑分组 -->
      <template #head-extra>
        <span class="table-head__hint">
          项目只是任务的逻辑分组：负责人 / 成员**不会**因此看到项目下的全部任务（可见性仍按数据范围 ∪ 参与关系）
        </span>
      </template>

      <!-- 负责人筛选：远程搜索用户，声明式下拉表达不了 -->
      <template #filter-owner_id="{ value, set }">
        <UserSelect
          :model-value="value"
          class="project-owner-filter"
          placeholder="负责人"
          @update:model-value="set"
        />
      </template>

      <template #toolbar>
        <t-button v-privilege="[PERM.projectCreate]" theme="primary" @click="openCreate">
          <template #icon><t-icon name="add" /></template>
          新建项目
        </t-button>
      </template>

        <template #code="{ row }">
          <span class="cell-code">{{ row.code }}</span>
        </template>

        <template #name="{ row }">
          <t-link theme="primary" @click="openDetail(row)">{{ row.name }}</t-link>
          <t-tag v-if="row.description" size="small" variant="light" class="cell-desc-tag">有说明</t-tag>
        </template>

        <template #owner_name="{ row }">{{ row.owner_name || '未指定' }}</template>

        <template #range="{ row }">
          <span class="cell-range">{{ row.start_date || '—' }} ~ {{ row.due_date || '—' }}</span>
        </template>

        <template #task_total="{ row }">
          <span>{{ row.task_total }}</span>
          <span class="cell-sub">（完成 {{ row.task_done }}）</span>
        </template>

        <template #completion_rate="{ row }">
          <div class="cell-progress">
            <t-progress :percentage="row.completion_rate" :label="false" theme="line" :stroke-width="6" />
            <span class="cell-progress__text">{{ row.completion_rate }}%</span>
          </div>
        </template>

        <template #status="{ row }">
          <t-tag size="small" :theme="row.status === 'ACTIVE' ? 'success' : 'default'" variant="light">
            {{ row.status === 'ACTIVE' ? '进行中' : '已归档' }}
          </t-tag>
        </template>

        <template #op="{ row }">
          <t-space size="small">
            <t-link theme="primary" @click="openDetail(row)">查看</t-link>
            <t-link v-privilege="[PERM.projectUpdate]" theme="primary" @click="openEdit(row)">编辑</t-link>
            <t-link v-privilege="[PERM.projectDelete]" theme="danger" @click="confirmRemove(row)">删除</t-link>
          </t-space>
        </template>

        <template #empty="{ filtered }">
          <div class="empty-state">
            <t-icon class="empty-state__icon" name="folder" />
            <p class="empty-state__title">{{ filtered ? '没有符合条件的项目' : '还没有项目' }}</p>
            <p class="empty-state__desc">
              {{ filtered ? '换个筛选条件试试，或点「重置」看全部' : '建一个项目，把相关任务归到一起去' }}
            </p>
            <t-button v-privilege="[PERM.projectCreate]" theme="primary" variant="outline" @click="openCreate">
              新建项目
            </t-button>
          </div>
        </template>
    </ListPage>
    <!-- 新建 / 编辑 -->
    <t-dialog
      v-model:visible="formVisible"
      :header="editingId ? '编辑项目' : '新建项目'"
      width="560px"
      :close-on-overlay-click="false"
      :confirm-btn="{ content: editingId ? '保存' : '创建', loading: saving }"
      @confirm="handleSubmit"
      @closed="resetForm"
    >
      <t-form ref="formRef" :data="form" :rules="rules" label-width="96px" class="project-form">
        <t-form-item label="项目编码" name="code">
          <t-input v-model="form.code" placeholder="如 PAY-2026（字母/数字/_/-，未删唯一）" :maxlength="32" />
        </t-form-item>
        <t-form-item label="项目名称" name="name">
          <t-input v-model="form.name" placeholder="一句话说清这个项目做什么" :maxlength="100" />
        </t-form-item>
        <t-form-item label="说明" name="description">
          <t-textarea v-model="form.description" :autosize="{ minRows: 2, maxRows: 5 }" :maxlength="20000" placeholder="可留空" />
        </t-form-item>
        <t-row :gutter="[16, 0]">
          <t-col :span="6">
            <t-form-item label="负责人" name="owner_id">
              <UserSelect v-model="form.owner_id" placeholder="留空 = 我自己" />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="状态" name="status">
              <t-select v-model="form.status" :options="statusOpts" />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="开始日" name="start_date">
              <t-date-picker v-model="form.start_date" clearable format="YYYY-MM-DD" value-type="YYYY-MM-DD" />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="结束日" name="due_date">
              <t-date-picker v-model="form.due_date" clearable format="YYYY-MM-DD" value-type="YYYY-MM-DD" />
            </t-form-item>
          </t-col>
        </t-row>
      </t-form>
    </t-dialog>

    <!-- 详情抽屉 -->
    <t-drawer v-model:visible="detailVisible" size="900px" :footer="false" header="项目详情" @closed="detailId = null">
      <template #body>
        <t-loading :loading="detailLoading" size="small">
          <div v-if="detail" class="detail">
            <div class="detail__head">
              <div class="detail__title">
                <span class="detail__name">{{ detail.name }}</span>
                <t-tag size="small" variant="outline">{{ detail.code }}</t-tag>
                <t-tag size="small" :theme="detail.status === 'ACTIVE' ? 'success' : 'default'" variant="light">
                  {{ detail.status === 'ACTIVE' ? '进行中' : '已归档' }}
                </t-tag>
              </div>
              <div class="detail__meta">
                负责人 {{ detail.owner_name || '未指定' }}
                <span class="detail__sep">·</span>
                {{ detail.start_date || '—' }} ~ {{ detail.due_date || '—' }}
                <span class="detail__sep">·</span>
                成员 {{ detail.member_count }} 人
              </div>
              <div v-if="detail.description" class="detail__desc">{{ detail.description }}</div>
            </div>

            <!-- 统计卡片（全部按「我可见的任务」统计） -->
            <div class="stat-grid">
              <div v-for="item in statCards" :key="item.label" class="stat-card">
                <span class="stat-card__label">{{ item.label }}</span>
                <span class="stat-card__value">{{ item.value }}</span>
              </div>
            </div>
            <div class="detail__progress">
              <span>完成率</span>
              <t-progress :percentage="detail.stats.completion_rate" :label="false" theme="line" :stroke-width="8" />
              <span class="detail__progress-text">{{ detail.stats.completion_rate }}%</span>
            </div>

            <!-- 成员 -->
            <div class="section">
              <div class="section__head">
                <span class="section__title">项目成员</span>
                <t-space size="small">
                  <UserSelect v-model="memberToAdd" placeholder="选择用户" />
                  <t-button size="small" variant="outline" :loading="memberSaving" @click="handleAddMember">添加</t-button>
                </t-space>
              </div>
              <div v-if="detail.members.length === 0" class="section__empty">还没有成员</div>
              <t-space v-else break-line size="small">
                <t-tag v-for="member in detail.members" :key="member.user_id" variant="light" closable @close="handleRemoveMember(member)">
                  {{ member.real_name || member.username }}
                </t-tag>
              </t-space>
              <p class="section__hint">成员只表示「参与这个项目」，不会因此看到项目下的全部任务。</p>
            </div>

            <!-- 项目下任务（走 T1 可见性） -->
            <div class="section">
              <div class="section__head">
                <span class="section__title">项目下任务（仅显示我有权查看的）</span>
                <span class="section__hint">共 {{ taskTotal }} 条</span>
              </div>
              <t-table
                row-key="id"
                size="small"
                :data="taskRows"
                :columns="taskColumns"
                :loading="taskLoading"
                :pagination="taskPagination"
                @page-change="handleTaskPageChange"
              >
                <template #task_status="{ row }">
                  <StatusBadge domain="task" :status="row.status" />
                </template>
                <template #task_due="{ row }">{{ row.due_date || '无期限' }}</template>
                <template #task_op="{ row }">
                  <t-link theme="primary" @click="openTask(row)">打开任务</t-link>
                </template>
                <template #empty>
                  <div class="section__empty">
                    这个项目下没有你能看见的任务（项目不是权限边界，别人负责的任务不会因项目而对你可见）
                  </div>
                </template>
              </t-table>
            </div>
          </div>
        </t-loading>
      </template>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * 项目分组列表页（菜单 `/project` → `business/project/index`，入口码 `business:project:read`）。
 *
 * # 这一页要回答的三个问题
 *
 * 1. **有哪些项目、各自进度如何** —— 表格列：编码 / 名称 / 负责人 / 起止 / 任务数 / 完成率；
 * 2. **某个项目里有什么任务** —— 详情抽屉复用 `GET /business/project/{id}/tasks`
 *    （后端走 T1 的可见性：项目负责人 / 成员看不到别人的任务）；
 * 3. **统计是不是我该看到的数字** —— 所有 `task_total` / `completion_rate` / `stats.*`
 *    都只统计**我可见的任务**，页面文案明确写出这一口径，避免用户以为是 bug。
 *
 * # 删除的语义（卡面二·删除二次确认）
 *
 * 删除按钮的确认框必须写清「任务不会被删除，只会移出项目」——
 * 这是最容易让用户误操作的一步（他可能以为删项目=清空任务）。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';
import ListPage from '@/components/ListPage/index.vue';
import type { FilterDef, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';
import UserSelect from '@/views/business/task/components/UserSelect.vue';
import StatusBadge from '@/views/business/task/components/StatusBadge.vue';
import { ApiError } from '@/api/request';
import {
  createProject,
  deleteProject,
  getProject,
  pageProjects,
  pageProjectTasks,
  updateProject,
  addProjectMember,
  removeProjectMember,
  type ProjectDetail,
  type ProjectItem,
  type ProjectMember,
  type ProjectPageQuery,
  type ProjectStatus,
} from '@/api/business/project-api';
import type { TaskCard } from '@/api/business/task-api';
import { PERM } from '@/constants/permissions';

const router = useRouter();
const route = useRoute();

/** 状态选项（与后端 `dto::STATUSES` 逐字一致） */
const statusOpts = [
  { label: '进行中', value: 'ACTIVE' },
  { label: '已归档', value: 'ARCHIVED' },
];

/** 表格数据 */
/** 每页条数（原来可选 10 / 20 / 50，交给组件分页器的每页条数开关） */
const PROJECT_PAGE_SIZE = 10;
/** ListPage 实例（保存 / 删除 / 详情里改任务后重拉） */
const listRef = ref<InstanceType<typeof ListPage> | null>(null);
/** 当前页码（`@loaded` 同步） */
const currentPage = ref(1);

/** 筛选条件 */
const query = reactive<{ name: string; status: ProjectStatus | ''; owner_id: string }>({
  name: '',
  status: '',
  owner_id: '',
});

/** 筛选项声明（关键词走回车、状态下拉选完点「查询」，与改造前一致） */
const filters: FilterDef[] = [
  { key: 'name', type: 'keyword', placeholder: '项目名称 / 编码', width: 240 },
  { key: 'status', type: 'select', placeholder: '状态', width: 150, options: statusOpts },
  { key: 'owner_id', type: 'custom' },
];

/** 拉一页 */
const fetcher: ListFetcher<ProjectItem> = (params) => pageProjects(params as ProjectPageQuery);

/** 入参拼装（沿用改造前的键名与「空串照传」口径） */
const queryBuilder: ListQueryBuilder = (_state, page, pageSize) => ({
  page,
  page_size: pageSize,
  name: query.name,
  status: query.status,
  owner_id: query.owner_id,
});

/** 一页拉完：同步页码 */
function onLoaded(payload: { page: number }): void {
  currentPage.value = payload.page;
}

/** 重拉到指定页（不传 = 当前页） */
async function reloadList(page: number = currentPage.value): Promise<void> {
  await listRef.value?.load(page);
}

/** 列定义：编码 / 名称 / 负责人 / 起止 / 任务数 / 完成率 / 状态 / 操作 */
const columns = [
  { colKey: 'code', title: '编码', width: 140 },
  { colKey: 'name', title: '项目名称', width: 220, ellipsis: true },
  { colKey: 'owner_name', title: '负责人', width: 100 },
  { colKey: 'range', title: '起止日期', width: 200 },
  { colKey: 'task_total', title: '任务数', width: 110 },
  { colKey: 'completion_rate', title: '完成率', width: 160 },
  { colKey: 'status', title: '状态', width: 96 },
  { colKey: 'op', title: '操作', width: 150, fixed: 'right' as const },
];

/** 表单可见性 / 编辑目标 */
const formVisible = ref(false);
const editingId = ref<string | null>(null);
const saving = ref(false);
const formRef = ref();

/** 表单模型（日期统一 `YYYY-MM-DD`，直接对齐后端 `NaiveDate`） */
interface ProjectFormModel {
  code: string;
  name: string;
  description: string;
  owner_id: string | null;
  status: ProjectStatus;
  start_date: string;
  due_date: string;
}

function emptyForm(): ProjectFormModel {
  return { code: '', name: '', description: '', owner_id: null, status: 'ACTIVE', start_date: '', due_date: '' };
}

const form = reactive<ProjectFormModel>(emptyForm());

/** 校验规则（编码字符集与长度与后端 `dto::normalize_code` 一致） */
const rules = {
  code: [
    { required: true, message: '请填写项目编码', trigger: 'blur' as const },
    {
      pattern: /^[A-Za-z0-9_-]{2,32}$/,
      message: '编码只能包含字母、数字、下划线、短横线，长度 2–32',
      trigger: 'blur' as const,
    },
  ],
  name: [
    { required: true, message: '请填写项目名称', trigger: 'blur' as const },
    { max: 100, message: '名称不能超过 100 字', trigger: 'blur' as const },
  ],
};

/** 详情抽屉 */
const detailVisible = ref(false);
const detailLoading = ref(false);
const detailId = ref<string | null>(null);
const detail = ref<ProjectDetail | null>(null);
/** 待添加的成员 */
const memberToAdd = ref<string | null>(null);
const memberSaving = ref(false);

/** 详情里的任务列表 */
const taskRows = ref<TaskCard[]>([]);
const taskLoading = ref(false);
const taskTotal = ref(0);
const taskPagination = reactive({ current: 1, pageSize: 10, total: 0, showJumper: false, pageSizeOptions: [10, 20] });
const taskColumns = [
  { colKey: 'task_no', title: '任务号', width: 130 },
  { colKey: 'title', title: '标题', ellipsis: true },
  { colKey: 'task_status', title: '状态', width: 100 },
  { colKey: 'owner_name', title: '负责人', width: 100 },
  { colKey: 'task_due', title: '截止日', width: 120 },
  { colKey: 'task_op', title: '操作', width: 100 },
];

/** 统计卡片（标签 + 值；工时统一折算成「小时」文案） */
const statCards = computed(() => {
  const stats = detail.value?.stats;
  if (!stats) {
    return [];
  }
  return [
    { label: '任务总数', value: String(stats.total) },
    { label: '待开始', value: String(stats.todo) },
    { label: '进行中', value: String(stats.doing) },
    { label: '待验收', value: String(stats.review) },
    { label: '已完成', value: String(stats.done) },
    { label: '已取消', value: String(stats.cancelled) },
    { label: '预估工时', value: `${stats.estimate_hours}h` },
    { label: '实际工时', value: `${stats.actual_hours}h` },
  ];
});


/** 打开新建 */
function openCreate(): void {
  editingId.value = null;
  Object.assign(form, emptyForm());
  formVisible.value = true;
}

/** 打开编辑（回填当前行，不额外请求详情） */
function openEdit(row: ProjectItem): void {
  editingId.value = row.id;
  Object.assign(form, {
    code: row.code,
    name: row.name,
    description: row.description,
    owner_id: row.owner_id,
    status: row.status,
    start_date: row.start_date ?? '',
    due_date: row.due_date ?? '',
  });
  formVisible.value = true;
}

function resetForm(): void {
  editingId.value = null;
  Object.assign(form, emptyForm());
  formRef.value?.clearValidate?.();
}

/** 提交（新建 / 编辑） */
async function handleSubmit(): Promise<void> {
  const result = await formRef.value?.validate?.();
  if (result !== true) {
    return;
  }
  saving.value = true;
  try {
    const description = form.description.trim();
    if (editingId.value) {
      // 编辑：可清空字段必须显式传 null（后端「缺席 = 不改」）
      await updateProject(editingId.value, {
        code: form.code.trim(),
        name: form.name.trim(),
        description: description ? description : null,
        owner_id: form.owner_id || null,
        status: form.status,
        start_date: form.start_date || null,
        due_date: form.due_date || null,
      });
      MessagePlugin.success('项目已更新');
    } else {
      await createProject({
        code: form.code.trim(),
        name: form.name.trim(),
        description: description || null,
        owner_id: form.owner_id || null,
        start_date: form.start_date || null,
        due_date: form.due_date || null,
      });
      MessagePlugin.success('项目已创建');
    }
    formVisible.value = false;
    await reloadList();
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '保存失败，请稍后重试');
  } finally {
    saving.value = false;
  }
}

/**
 * 删除二次确认。
 *
 * 文案是卡面硬要求：必须让用户知道**任务不会被删除，只会移出项目**。
 * 确认框里带 `unbound_tasks` 的回执数字（后端返回真实解绑条数），比「已删除」更有信息量。
 */
function confirmRemove(row: ProjectItem): void {
  const dialog = DialogPlugin.confirm({
    header: `删除项目「${row.name}」`,
    body: `任务不会被删除，只会移出项目（project_id 置空，任务仍留在任务列表里）。确定删除吗？`,
    theme: 'warning',
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: async () => {
      try {
        const result = await deleteProject(row.id);
        MessagePlugin.success(`项目已删除，${result.unbound_tasks} 个任务已移出项目（任务本身未删除）`);
        dialog.hide();
        await reloadList();
      } catch (error) {
        MessagePlugin.error(error instanceof ApiError ? error.message : '删除失败，请稍后重试');
      }
    },
  });
}

/** 打开详情抽屉（`row` 只用来拿 id —— 详情数据一律重新拉，避免用列表里的旧数字） */
async function openDetail(row: Pick<ProjectItem, 'id'>): Promise<void> {
  detailId.value = row.id;
  detailVisible.value = true;
  memberToAdd.value = null;
  taskPagination.current = 1;
  await Promise.all([loadDetail(), loadProjectTasks()]);
}

/** 拉详情（头部 + 统计 + 成员） */
async function loadDetail(): Promise<void> {
  if (!detailId.value) {
    return;
  }
  detailLoading.value = true;
  try {
    detail.value = await getProject(detailId.value);
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '项目详情加载失败');
    detailVisible.value = false;
  } finally {
    detailLoading.value = false;
  }
}

/** 拉项目下任务（后端走 T1 可见性） */
async function loadProjectTasks(): Promise<void> {
  if (!detailId.value) {
    return;
  }
  taskLoading.value = true;
  try {
    const data = await pageProjectTasks(detailId.value, {
      page: taskPagination.current,
      page_size: taskPagination.pageSize,
    });
    taskRows.value = data.list;
    taskTotal.value = data.total;
    taskPagination.total = data.total;
  } catch (error) {
    taskRows.value = [];
    taskTotal.value = 0;
    MessagePlugin.error(error instanceof ApiError ? error.message : '项目下任务加载失败');
  } finally {
    taskLoading.value = false;
  }
}

function handleTaskPageChange(pageInfo: { current?: number; pageSize?: number }): void {
  taskPagination.current = pageInfo.current ?? 1;
  taskPagination.pageSize = pageInfo.pageSize ?? taskPagination.pageSize;
  void loadProjectTasks();
}

/** 加成员 */
async function handleAddMember(): Promise<void> {
  if (!detailId.value || !memberToAdd.value) {
    MessagePlugin.warning('请先选择要添加的用户');
    return;
  }
  memberSaving.value = true;
  try {
    const result = await addProjectMember(detailId.value, memberToAdd.value);
    MessagePlugin.success(result.changed ? '成员已添加' : '该用户已经是项目成员');
    memberToAdd.value = null;
    await loadDetail();
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '添加成员失败');
  } finally {
    memberSaving.value = false;
  }
}

/** 移除成员 */
async function handleRemoveMember(member: ProjectMember): Promise<void> {
  if (!detailId.value) {
    return;
  }
  try {
    await removeProjectMember(detailId.value, member.user_id);
    MessagePlugin.success('成员已移除');
    await loadDetail();
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '移除成员失败');
  }
}

/** 打开任务（跳到任务页并带项目筛选，用户能在完整列表里继续操作） */
function openTask(row: TaskCard): void {
  void router.push({ path: '/task', query: { project_id: row.project_id ?? '' } });
}

onMounted(() => {
  void reloadList();
  // 深链：任务页 / 任务详情点「项目」会带 `?id=<uuid>` 过来，直接展开该项目的详情抽屉
  const deepLinkId = typeof route.query.id === 'string' ? route.query.id : '';
  if (deepLinkId) {
    void openDetail({ id: deepLinkId });
  }
});
</script>

<style scoped lang="less">
.project-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.table-head {
  display: flex;
  align-items: baseline;
  gap: 12px;
  margin-bottom: 12px;

  &__title {
    font-size: 16px;
    font-weight: 600;
  }

  &__hint {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.project-owner-filter {
  width: 150px;
}

.cell-code {
  font-family: var(--app-font-mono, monospace);
}

.cell-sub {
  margin-left: 4px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.cell-range {
  font-size: 13px;
}

.cell-desc-tag {
  margin-left: 6px;
}

.cell-progress {
  display: flex;
  align-items: center;
  gap: 8px;

  &__text {
    min-width: 44px;
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }
}

.empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 6px;
  padding: 32px 0;

  &__icon {
    font-size: 32px;
    color: var(--td-text-color-placeholder);
  }

  &__title {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
  }

  &__desc {
    margin: 0 0 8px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.detail {
  display: flex;
  flex-direction: column;
  gap: 16px;

  &__head {
    padding: 12px 16px;
    border-radius: 8px;
    background: var(--td-bg-color-container-hover);
  }

  &__title {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  &__name {
    font-size: 18px;
    font-weight: 600;
  }

  &__meta {
    margin-top: 6px;
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }

  &__sep {
    margin: 0 6px;
  }

  &__desc {
    margin-top: 8px;
    font-size: 13px;
    color: var(--td-text-color-primary);
    white-space: pre-wrap;
  }

  &__progress {
    display: flex;
    align-items: center;
    gap: 12px;
    font-size: 13px;
  }

  &__progress-text {
    min-width: 48px;
    font-weight: 600;
  }
}

.stat-grid {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 12px;
}

.stat-card {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 12px 16px;
  border-radius: 8px;
  background: var(--td-bg-color-container-hover);

  &__label {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__value {
    font-size: 20px;
    font-weight: 600;
  }
}

.section {
  display: flex;
  flex-direction: column;
  gap: 8px;

  &__head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  &__title {
    font-size: 14px;
    font-weight: 600;
  }

  &__empty {
    padding: 12px;
    font-size: 13px;
    color: var(--td-text-color-placeholder);
  }

  &__hint {
    margin: 0;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.project-form {
  :deep(.t-form__item) {
    margin-bottom: 16px;
  }
}
</style>

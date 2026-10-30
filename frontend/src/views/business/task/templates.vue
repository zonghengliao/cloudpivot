<template>
  <div class="template-page">
    <!-- 筛选区 -->
    <t-card :bordered="false" class="filter-card">
      <t-row :gutter="[16, 12]" align="middle">
        <t-col :span="6">
          <t-input v-model="query.keyword" placeholder="模板名 / 任务标题关键词" clearable @enter="loadList">
            <template #prefix-icon><t-icon name="search" /></template>
          </t-input>
        </t-col>
        <t-col :span="4">
          <t-space align="center">
            <t-switch v-model="query.enabledOnly" size="small" @change="loadList" />
            <span class="hint">只看已开启周期的模板</span>
          </t-space>
        </t-col>
        <t-col :span="14">
          <t-space>
            <t-button theme="primary" @click="loadList">
              <template #icon><t-icon name="search" /></template>
              查询
            </t-button>
            <t-button variant="outline" @click="handleReset">重置</t-button>
            <t-button v-privilege="[PERM.taskCreate]" variant="outline" theme="primary" @click="openCreate">
              <template #icon><t-icon name="add" /></template>
              新建模板
            </t-button>
            <t-link theme="primary" @click="goTaskList">返回任务列表</t-link>
          </t-space>
        </t-col>
      </t-row>
    </t-card>

    <!-- 列表 -->
    <t-card :bordered="false" class="table-card">
      <div class="table-head">
        <span class="table-head__title">任务模板（{{ rows.length }} 个）</span>
        <span class="table-head__hint">
          模板是「默认值集合」：一键建任务会带上子任务；开启周期后由调度器每 5 分钟按 cron（北京时间）自动生成
        </span>
      </div>

      <t-alert v-if="errorMessage" class="table-alert" theme="error" :message="errorMessage">
        <template #operation>
          <t-button size="small" variant="outline" @click="loadList">重试</t-button>
        </template>
      </t-alert>

      <t-table row-key="id" :data="rows" :columns="columns" :loading="loading" :hover="true" stripe>
        <template #name="{ row }">
          <t-link theme="primary" @click="openEdit(row)">{{ row.name }}</t-link>
        </template>

        <template #priority="{ row }">
          <t-tag size="small" variant="light" :theme="priorityTheme(row.priority)">{{ row.priority }}</t-tag>
        </template>

        <template #subtasks="{ row }">
          <span>{{ row.subtasks.length }} 条</span>
          <span v-if="row.subtasks.length" class="cell-sub">
            （{{ row.subtasks.map((item: TaskTemplateSubtask) => item.title).join(' / ') }}）
          </span>
        </template>

        <template #cron_expr="{ row }">
          <span v-if="row.cron_expr" class="cell-cron">{{ row.cron_expr }}</span>
          <span v-else class="cell-muted">—</span>
          <t-tag v-if="row.enabled" class="cell-tag" size="small" theme="success" variant="light">周期中</t-tag>
        </template>

        <template #next_run_at="{ row }">
          <span v-if="row.enabled && row.next_run_at">{{ formatTime(row.next_run_at) }}</span>
          <span v-else class="cell-muted">—</span>
        </template>

        <template #last_run_at="{ row }">
          <span v-if="row.last_run_at">{{ formatTime(row.last_run_at) }}</span>
          <span v-else class="cell-muted">未生成过</span>
        </template>

        <template #op="{ row }">
          <t-space size="small">
            <t-link v-privilege="[PERM.taskCreate]" theme="primary" @click="useTemplate(row)">建任务</t-link>
            <t-link v-privilege="[PERM.taskUpdate]" theme="primary" @click="openEdit(row)">编辑</t-link>
            <t-link v-privilege="[PERM.taskDelete]" theme="danger" @click="confirmRemove(row)">删除</t-link>
          </t-space>
        </template>

        <template #empty>
          <div class="empty-state">
            <t-icon class="empty-state__icon" name="file-copy" />
            <p class="empty-state__title">还没有任务模板</p>
            <p class="empty-state__desc">把「每周都要做的那些事」存成模板，下次一键建任务（含子任务、可开周期）</p>
            <t-button v-privilege="[PERM.taskCreate]" theme="primary" variant="outline" @click="openCreate">
              新建模板
            </t-button>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- 新建 / 编辑 -->
    <t-dialog
      v-model:visible="formVisible"
      :header="editingId ? '编辑模板' : '新建模板'"
      width="720px"
      :close-on-overlay-click="false"
      :confirm-btn="{ content: editingId ? '保存' : '创建', loading: saving }"
      @confirm="handleSave"
      @closed="resetForm"
    >
      <t-form ref="formRef" :data="form" :rules="rules" label-width="104px">
        <t-row :gutter="[16, 0]">
          <t-col :span="12">
            <t-form-item label="模板名" name="name">
              <t-input v-model="form.name" placeholder="如：周报（≤100 字，不可重名）" :maxlength="100" />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="任务标题" name="title">
              <t-input v-model="form.title" placeholder="生成任务的标题（≤200 字）" :maxlength="200" />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="描述">
              <t-textarea
                v-model="form.description"
                placeholder="验收标准 / 参考链接（可留空）"
                :autosize="{ minRows: 2, maxRows: 6 }"
                :maxlength="20000"
              />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="优先级">
              <t-select v-model="form.priority" :options="priorityOpts" />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="预估工时">
              <t-input-number
                v-model="estimateProxy"
                theme="normal"
                :min="0"
                :max="9999"
                :step="0.5"
                :decimal-places="1"
                placeholder="小时"
              />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="负责人">
              <UserSelect v-model="form.owner_id" placeholder="留空 = 模板创建人" clearable />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="所属项目">
              <t-select
                v-model="form.project_id"
                :options="projectOptions"
                :loading="projectLoading"
                placeholder="可不选"
                clearable
                filterable
              />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="需要验收">
              <t-switch v-model="form.need_review" />
              <span class="hint">开启后生成的任务要先验收才能完成</span>
            </t-form-item>
          </t-col>

          <!-- 子任务 -->
          <t-col :span="12">
            <t-form-item label="子任务">
              <div class="subtask-editor">
                <div v-for="(item, index) in form.subtasks" :key="index" class="subtask-row">
                  <t-input v-model="item.title" placeholder="子任务标题" :maxlength="200" />
                  <t-select v-model="item.priority" :options="priorityOpts" class="subtask-row__priority" />
                  <t-button variant="text" theme="danger" size="small" @click="removeSubtask(index)">删除</t-button>
                </div>
                <t-button variant="outline" size="small" :disabled="form.subtasks.length >= 20" @click="addSubtask">
                  <template #icon><t-icon name="add" /></template>
                  添加子任务（{{ form.subtasks.length }}/20）
                </t-button>
              </div>
            </t-form-item>
          </t-col>

          <!-- 周期 -->
          <t-col :span="12">
            <t-form-item label="周期任务">
              <div class="cron-editor">
                <t-switch v-model="form.enabled" />
                <span class="hint">开启后按下面的 cron（北京时间）自动生成任务，来源标记 RECURRING</span>
                <div class="cron-editor__row">
                  <t-select
                    class="cron-editor__examples"
                    :options="cronExamples"
                    placeholder="常见示例（选中即填入）"
                    @change="applyCronExample"
                  />
                  <t-input v-model="form.cron_expr" placeholder="标准 cron：分 时 日 月 周，如 0 9 * * *" :maxlength="100" />
                </div>
                <span class="hint">示例：每天 9 点 = <code>0 9 * * *</code>；每周一 9 点 = <code>0 9 * * 1</code>；每月 1 号 9 点 = <code>0 9 1 * *</code></span>
              </div>
            </t-form-item>
          </t-col>
        </t-row>
      </t-form>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 任务模板管理（批次 E · E11）。
 *
 * # 页面职责
 *
 * - 模板 CRUD（`/api/business/task/templates`、`/api/business/task/template/{id}`）；
 * - 「建任务」= `POST /api/business/task/from-template/{id}`（含子任务，一次落库）；
 * - 周期开关 + cron（标准 5 段，北京时间）。
 *
 * # 三个前端约定
 *
 * 1. **cron 不做本地校验**：后端 `recurring::parse_cron` 是唯一权威，
 *    前端只提供常见示例下拉，非法表达式由后端返 `63503` 并给中文原因；
 * 2. **可空下拉不用 `null`**：TDesign 的 `t-select` / `t-input-number` 不吃 `null`
 *    （`vue-tsc` 会报 TS2322），表单模型一律用空串 / `undefined`；
 * 3. **权限沿用任务域四个码**（`business:task:*`），模板没有独立权限点 ——
 *    后端注解就是这么写的，前端不要发明第五个。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin, DialogPlugin } from 'tdesign-vue-next';
import { useRouter } from 'vue-router';
import dayjs from 'dayjs';
import UserSelect from './components/UserSelect.vue';
import {
  createTaskFromTemplate,
  createTaskTemplate,
  deleteTaskTemplate,
  listTaskTemplates,
  updateTaskTemplate,
  type TaskTemplate,
  type TaskTemplateSaveReq,
  type TaskTemplateSubtask,
} from '@/api/business/task-api';
import { pageProjects } from '@/api/business/project-api';
import { priorityOptions } from '@/constants/status-map';
import { PERM } from '@/constants/permissions';
import { ApiError } from '@/api/request';

const router = useRouter();

/** 筛选条件 */
const query = reactive({ keyword: '', enabledOnly: false });

/** 列表数据 */
const rows = ref<TaskTemplate[]>([]);
/** 列表加载中 */
const loading = ref(false);
/** 列表错误文案 */
const errorMessage = ref('');

/** 优先级选项 */
const priorityOpts = priorityOptions();

/** 常见 cron 示例（前端只做「填值」，校验在后端） */
const cronExamples = [
  { label: '每天 9 点', value: '0 9 * * *' },
  { label: '每周一 9 点', value: '0 9 * * 1' },
  { label: '每月 1 号 9 点', value: '0 9 1 * *' },
  { label: '每小时整点', value: '0 * * * *' },
  { label: '工作日 9 点', value: '0 9 * * 1-5' },
];

/** 表格列 */
const columns = [
  { colKey: 'name', title: '模板名', width: 160 },
  { colKey: 'title', title: '任务标题', width: 200, ellipsis: true },
  { colKey: 'priority', title: '优先级', width: 90 },
  { colKey: 'subtasks', title: '子任务', width: 220, ellipsis: true },
  { colKey: 'cron_expr', title: '周期（cron）', width: 180 },
  { colKey: 'next_run_at', title: '下次生成', width: 170 },
  { colKey: 'last_run_at', title: '上次生成', width: 170 },
  { colKey: 'op', title: '操作', width: 180, fixed: 'right' as const },
];

/** 项目下拉 */
const projectOptions = ref<Array<{ label: string; value: string }>>([]);
/** 项目下拉加载中 */
const projectLoading = ref(false);

/** 表单模型 */
interface TemplateFormModel {
  name: string;
  title: string;
  description: string;
  priority: string;
  estimate_hours: number | undefined;
  need_review: boolean;
  subtasks: Array<{ title: string; priority: string }>;
  owner_id: string;
  project_id: string;
  cron_expr: string;
  enabled: boolean;
}

/** 空表单 */
function emptyForm(): TemplateFormModel {
  return {
    name: '',
    title: '',
    description: '',
    priority: 'P2',
    estimate_hours: undefined,
    need_review: false,
    subtasks: [],
    owner_id: '',
    project_id: '',
    cron_expr: '',
    enabled: false,
  };
}

const form = reactive<TemplateFormModel>(emptyForm());
const formVisible = ref(false);
const formRef = ref();
const editingId = ref('');
const saving = ref(false);

/** `t-input-number` 不吃 `null`：用 undefined 表达「不填」 */
const estimateProxy = computed<number | undefined>({
  get: () => form.estimate_hours,
  set: (value) => {
    form.estimate_hours = value;
  },
});

/** 表单规则 */
const rules = {
  name: [{ required: true, message: '模板名必填', type: 'error' as const }],
  title: [{ required: true, message: '任务标题必填', type: 'error' as const }],
};

/** 优先级配色 */
function priorityTheme(priority: string): 'danger' | 'warning' | 'primary' | 'default' {
  if (priority === 'P0') return 'danger';
  if (priority === 'P1') return 'warning';
  if (priority === 'P2') return 'primary';
  return 'default';
}

/** 时间格式化（后端下发 RFC3339 UTC） */
function formatTime(raw: string): string {
  const parsed = dayjs(raw);
  return parsed.isValid() ? parsed.format('YYYY-MM-DD HH:mm') : raw;
}

/** 拉列表 */
async function loadList(): Promise<void> {
  loading.value = true;
  errorMessage.value = '';
  try {
    const result = await listTaskTemplates({
      keyword: query.keyword,
      enabled_only: query.enabledOnly,
    });
    rows.value = result.list;
  } catch (error) {
    errorMessage.value = error instanceof ApiError ? error.message : '加载失败，请重试';
  } finally {
    loading.value = false;
  }
}

/** 重置筛选 */
function handleReset(): void {
  query.keyword = '';
  query.enabledOnly = false;
  void loadList();
}

/** 拉项目下拉 */
async function loadProjects(): Promise<void> {
  projectLoading.value = true;
  try {
    const page = await pageProjects({ page: 1, page_size: 200 });
    projectOptions.value = page.list.map((item) => ({ label: item.name, value: item.id }));
  } catch {
    projectOptions.value = [];
  } finally {
    projectLoading.value = false;
  }
}

/** 打开新建 */
function openCreate(): void {
  editingId.value = '';
  Object.assign(form, emptyForm());
  formVisible.value = true;
}

/** 打开编辑 */
function openEdit(row: TaskTemplate): void {
  editingId.value = row.id;
  Object.assign(form, {
    name: row.name,
    title: row.title,
    description: row.description ?? '',
    priority: row.priority,
    estimate_hours: row.estimate_hours ?? undefined,
    need_review: row.need_review,
    subtasks: row.subtasks.map((item) => ({ title: item.title, priority: item.priority })),
    owner_id: row.owner_id ?? '',
    project_id: row.project_id ?? '',
    cron_expr: row.cron_expr ?? '',
    enabled: row.enabled,
  });
  formVisible.value = true;
}

/** 关闭后复位（避免下次新建带上上次的值） */
function resetForm(): void {
  editingId.value = '';
  Object.assign(form, emptyForm());
}

/** 加一条子任务 */
function addSubtask(): void {
  if (form.subtasks.length >= 20) return;
  form.subtasks.push({ title: '', priority: form.priority });
}

/** 删一条子任务 */
function removeSubtask(index: number): void {
  form.subtasks.splice(index, 1);
}

/** 选中 cron 示例 */
function applyCronExample(value: unknown): void {
  if (typeof value === 'string') {
    form.cron_expr = value;
  }
}

/** 保存（新建 / 编辑） */
async function handleSave(): Promise<void> {
  const validated = await formRef.value?.validate?.();
  if (validated !== true) return;
  const payload: TaskTemplateSaveReq = {
    name: form.name.trim(),
    title: form.title.trim(),
    description: form.description.trim() || null,
    priority: form.priority,
    estimate_hours: form.estimate_hours ?? null,
    need_review: form.need_review,
    subtasks: form.subtasks
      .filter((item) => item.title.trim())
      .map<TaskTemplateSubtask>((item) => ({ title: item.title.trim(), priority: item.priority as never })),
    owner_id: form.owner_id || null,
    project_id: form.project_id || null,
    cron_expr: form.cron_expr.trim() || null,
    enabled: form.enabled,
  };
  saving.value = true;
  try {
    if (editingId.value) {
      await updateTaskTemplate(editingId.value, payload);
      void MessagePlugin.success('模板已保存');
    } else {
      await createTaskTemplate(payload);
      void MessagePlugin.success('模板已创建');
    }
    formVisible.value = false;
    await loadList();
  } catch (error) {
    const message = error instanceof ApiError ? error.message : '保存失败，请重试';
    void MessagePlugin.error(message);
  } finally {
    saving.value = false;
  }
}

/** 删除（二次确认） */
function confirmRemove(row: TaskTemplate): void {
  const dialog = DialogPlugin.confirm({
    header: '删除模板',
    body: `确定删除模板「${row.name}」？已按它生成的任务不受影响。`,
    theme: 'warning',
    onConfirm: async () => {
      try {
        await deleteTaskTemplate(row.id);
        void MessagePlugin.success('模板已删除');
        await loadList();
      } catch (error) {
        const message = error instanceof ApiError ? error.message : '删除失败，请重试';
        void MessagePlugin.error(message);
      }
      dialog.hide();
    },
  });
}

/** 一键建任务（含子任务） */
async function useTemplate(row: TaskTemplate): Promise<void> {
  try {
    const created = await createTaskFromTemplate(row.id, {});
    void MessagePlugin.success(`已创建任务 ${created.task_no}（含 ${created.subtask_ids.length} 条子任务）`);
    await loadList();
    void router.push({ path: '/task/list', query: { id: created.id } });
  } catch (error) {
    const message = error instanceof ApiError ? error.message : '建任务失败，请重试';
    void MessagePlugin.error(message);
  }
}

/** 回任务列表 */
function goTaskList(): void {
  void router.push('/task/list');
}

onMounted(() => {
  void loadList();
  void loadProjects();
});
</script>

<style scoped>
.template-page {
  padding: 16px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.filter-card,
.table-card {
  border-radius: var(--td-radius-medium);
}

.table-head {
  display: flex;
  align-items: baseline;
  gap: 12px;
  margin-bottom: 12px;
}

.table-head__title {
  font-weight: 600;
  font-size: 15px;
}

.table-head__hint {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.table-alert {
  margin-bottom: 12px;
}

.hint {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
  margin-left: 8px;
}

.cell-sub {
  color: var(--td-text-color-secondary);
  font-size: 12px;
  margin-left: 4px;
}

.cell-muted {
  color: var(--td-text-color-placeholder);
}

.cell-cron {
  font-family: var(--td-font-family-mono);
  font-size: 12px;
}

.cell-tag {
  margin-left: 6px;
}

.subtask-editor {
  display: flex;
  flex-direction: column;
  gap: 8px;
  width: 100%;
}

.subtask-row {
  display: flex;
  gap: 8px;
  align-items: center;
}

.subtask-row__priority {
  width: 130px;
  flex: none;
}

.cron-editor {
  display: flex;
  flex-direction: column;
  gap: 6px;
  width: 100%;
}

.cron-editor__row {
  display: flex;
  gap: 8px;
  align-items: center;
}

.cron-editor__examples {
  width: 200px;
  flex: none;
}

.empty-state {
  padding: 32px 0;
  text-align: center;
}

.empty-state__icon {
  font-size: 40px;
  color: var(--td-text-color-placeholder);
}

.empty-state__title {
  margin: 8px 0 4px;
  font-weight: 600;
}

.empty-state__desc {
  margin: 0 0 12px;
  color: var(--td-text-color-secondary);
  font-size: 13px;
}
</style>

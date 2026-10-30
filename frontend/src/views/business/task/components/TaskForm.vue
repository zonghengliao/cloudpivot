<template>
  <t-dialog
    v-model:visible="innerVisible"
    :header="isEdit ? '编辑任务' : '新建任务'"
    width="620px"
    :close-on-overlay-click="false"
    :confirm-btn="{ content: isEdit ? '保存' : '创建', loading: saving }"
    @confirm="handleSubmit"
    @closed="handleClosed"
  >
    <t-loading :loading="detailLoading" size="small">
      <t-form ref="formRef" :data="form" :rules="rules" label-width="96px" class="task-form">
        <t-form-item v-if="!isEdit" label="从模板创建">
          <t-select
            v-model="templateProxy"
            :options="templateOptions"
            :loading="templateLoading"
            placeholder="不选 = 自己填"
            clearable
            filterable
            @change="handleTemplateChange"
          />
          <span v-if="selectedTemplate" class="form-hint">
            将按模板生成 {{ selectedTemplate.subtasks.length }} 条子任务；优先级 / 描述 / 验收开关取自模板
          </span>
        </t-form-item>
        <t-form-item label="标题" name="title">
          <t-input v-model="form.title" placeholder="一句话说清要做什么（200 字以内）" :maxlength="200" autofocus />
        </t-form-item>
        <t-form-item label="描述" name="description">
          <t-textarea
            v-model="form.description"
            placeholder="补充上下文、验收标准、参考链接（可留空）"
            :autosize="{ minRows: 3, maxRows: 8 }"
            :maxlength="20000"
          />
        </t-form-item>
        <t-row :gutter="[16, 0]">
          <t-col :span="6">
            <t-form-item label="优先级" name="priority">
              <t-select v-model="form.priority" :options="priorityOpts" />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="截止日期" name="due_date">
              <t-date-picker
                v-model="form.due_date"
                clearable
                format="YYYY-MM-DD"
                value-type="YYYY-MM-DD"
                :placeholder="isEdit ? '留空表示无期限' : '留空默认本周五'"
              />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="负责人" name="owner_id">
              <UserSelect v-model="form.owner_id" :clearable="!isEdit" placeholder="留空 = 我自己" />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="预估工时" name="estimate_hours">
              <t-input-number
                v-model="estimateHoursProxy"
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
            <t-form-item label="所属项目" name="project_id">
              <t-select
                v-model="projectProxy"
                :options="projectOptions"
                :loading="projectLoading"
                placeholder="可不选（移出项目）"
                clearable
                filterable
              />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="父任务" name="parent_id">
              <!-- 只在新建时可选：编辑改父级 = 改结构，容易乱（卡面约定） -->
              <t-select
                v-if="!isEdit"
                v-model="parentProxy"
                :options="parentOptions"
                :loading="parentLoading"
                placeholder="可不选（作为顶层任务）"
                clearable
                filterable
                @change="handleParentChange"
              />
              <span v-else class="form-readonly">{{ editParentLabel }}</span>
              <span v-if="!isEdit && form.parent_id && parentDue" class="form-hint">
                父任务截止 {{ parentDue }}
              </span>
              <span v-if="!isEdit && dueAfterParent" class="form-hint form-hint--warn">
                晚于父任务截止日，改期时会被 63112 拦下
              </span>
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="需要验收" name="need_review">
              <t-switch v-model="form.need_review" />
              <span class="form-hint">开启后完成时需有人验收才转「已完成」</span>
            </t-form-item>
          </t-col>
        </t-row>
      </t-form>
    </t-loading>
  </t-dialog>
</template>

<script setup lang="ts">
/**
 * 任务新建 / 编辑表单（`business:task:create` / `business:task:update`）。
 *
 * # 两个后端契约（不遵守就 400 或被静默忽略）
 *
 * 1. **编辑语义「缺席 ≠ 清空」**：`TaskUpdateReq` 里被清空的可空字段必须显式传 `null`
 *    （描述 / 截止日 / 预估工时），省掉字段 = 保持原值；
 * 2. **负责人不可清空**：把负责人改成「空」在后端等于「不改」（转派语义属 T3），
 *    所以编辑态 `UserSelect` 不给清空，避免用户以为清掉了其实没变。
 */
import { computed, reactive, ref, watch } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import dayjs from 'dayjs';
import UserSelect from './UserSelect.vue';
import { ApiError } from '@/api/request';
import {
  createTask,
  createTaskFromTemplate,
  getTask,
  listTaskTemplates,
  pageTasks,
  updateTask,
  type TaskCreateReq,
  type TaskTemplate,
  type TaskUpdateReq,
} from '@/api/business/task-api';
import { pageProjects } from '@/api/business/project-api';
import { priorityOptions } from '@/constants/status-map';

const props = defineProps<{
  /** 是否可见（v-model） */
  visible: boolean;
  /** 编辑的任务 id；null 表示新建 */
  taskId?: string | null;
}>();

const emit = defineEmits<{
  /** 可见性变化 */
  (event: 'update:visible', value: boolean): void;
  /** 保存成功（参数为任务 id，父组件据此刷新列表） */
  (event: 'saved', id: string): void;
}>();

/** 表单数据（日期统一 `YYYY-MM-DD` 字符串，直接对齐后端 `NaiveDate`） */
interface TaskFormModel {
  /** 标题 */
  title: string;
  /** 描述 */
  description: string;
  /** 优先级 */
  priority: string;
  /** 负责人 id */
  owner_id: string | null;
  /** 截止日期 */
  due_date: string;
  /** 预估工时 */
  estimate_hours: number | null;
  /** 是否需验收 */
  need_review: boolean;
  /** 所属项目 id（T7c；null = 不归入项目） */
  project_id: string | null;
  /** 父任务 id（E6b；仅新建可选，null = 顶层任务） */
  parent_id: string | null;
  /** 模板 id（E11；仅新建可选，null = 不按模板建） */
  template_id: string | null;
}

/** 优先级选项（P0 最高 → P3 最低，文案来自 status-map） */
const priorityOpts = priorityOptions();

/** 项目下拉选项（打开表单时拉一次；项目数量不大，一次取 200 条足够） */
const projectOptions = ref<Array<{ label: string; value: string }>>([]);
/** 项目下拉加载中 */
const projectLoading = ref(false);

/**
 * 拉项目下拉选项。
 *
 * 只显示**归档项目也包含**：历史任务可能还挂在已归档项目上，编辑时若下拉里没有它，
 * t-select 会显示成裸 id（看起来像 bug）。项目页负责展示「已归档」标签。
 */
async function loadProjectOptions(): Promise<void> {
  projectLoading.value = true;
  try {
    const data = await pageProjects({ page: 1, page_size: 200 });
    projectOptions.value = data.list.map((item) => ({
      label: `${item.code} ${item.name}`,
      value: item.id,
    }));
  } catch {
    // 拉不到项目选项不该挡住建任务：任务可以不归入项目
    projectOptions.value = [];
  } finally {
    projectLoading.value = false;
  }
}

/** 表单实例 */
const formRef = ref();

// ==================== 父任务选择（E6b） ====================

/** 父任务下拉选项（一次取 100 条；不排除「已有子任务」的任务，允许三级） */
const parentOptions = ref<Array<{ label: string; value: string }>>([]);
/** 父任务下拉加载中 */
const parentLoading = ref(false);
/** 父任务 id → 截止日（选中父任务后用于「子任务不得晚于父任务」前端提示） */
const parentDueMap = ref<Record<string, string | null>>({});
/** 编辑态回显的父任务编号（详情接口只下发 `parent_task_no`） */
const editParentNo = ref('');

/**
 * 拉父任务下拉。
 *
 * 用列表接口而不是新接口：父任务候选就是「我能看见的任务」，`GET /task/page` 的可见性
 * 已经按数据范围 ∪ 参与关系过滤，取前 100 条够用（超出时下拉里搜不到，属已知取舍）。
 */
async function loadParentOptions(): Promise<void> {
  parentLoading.value = true;
  try {
    const data = await pageTasks({ page: 1, page_size: 100 });
    const dues: Record<string, string | null> = {};
    parentOptions.value = data.list.map((item) => {
      dues[item.id] = item.due_date;
      return { label: `${item.task_no} ${item.title}`, value: item.id };
    });
    parentDueMap.value = dues;
  } catch {
    // 拉不到候选不该挡住建任务：不选父任务即可
    parentOptions.value = [];
    parentDueMap.value = {};
  } finally {
    parentLoading.value = false;
  }
}

/** 父任务双向代理（`t-select` 不吃 null，与所属项目同款转换） */
const parentProxy = computed<string | undefined>({
  get: () => form.parent_id ?? undefined,
  set: (value) => {
    form.parent_id = typeof value === 'string' && value ? value : null;
  },
});

/** 已选父任务的截止日（空串 = 无期限 / 未选） */
const parentDue = computed(() => (form.parent_id ? parentDueMap.value[form.parent_id] ?? '' : ''));

/** 子任务截止日是否晚于父任务（只提示，不阻断保存，原因见 validateDueDate 注释） */
const dueAfterParent = computed(() => {
  if (!form.due_date || !parentDue.value) {
    return false;
  }
  return dayjs(form.due_date).startOf('day').isAfter(dayjs(parentDue.value).startOf('day'));
});

/** 编辑态只读展示的父任务（优先下拉里的「编号 标题」，退化为详情里的编号） */
const editParentLabel = computed(() => {
  if (!form.parent_id) {
    return '—（顶层任务）';
  }
  const hit = parentOptions.value.find((item) => item.value === form.parent_id);
  return hit ? hit.label : editParentNo.value || '父任务（已不在候选列表）';
});

/** 选完父任务：父任务截止日变了，截止日的提示要重算（`dueAfterParent` 是 computed，无需额外动作） */
function handleParentChange(): void {
  formRef.value?.clearValidate?.('due_date');
}

/** 保存中 */
const saving = ref(false);
/** 详情加载中（编辑回显） */
const detailLoading = ref(false);

/** 默认值：优先级 P2、无需验收、其余空（与后端「两秒建任务」默认值一致） */
function emptyForm(): TaskFormModel {
  return {
    title: '',
    description: '',
    priority: 'P2',
    owner_id: null,
    due_date: '',
    estimate_hours: null,
    need_review: false,
    project_id: null,
    parent_id: null,
    template_id: null,
  };
}

const form = reactive<TaskFormModel>(emptyForm());

/** 模板下拉选项（E11；新建时懒加载一次） */
const templateOptions = ref<Array<{ label: string; value: string }>>([]);
/** 模板列表（保存完整对象：选中后要回填字段与子任务数） */
const templateList = ref<TaskTemplate[]>([]);
/** 模板下拉加载中 */
const templateLoading = ref(false);
/** 当前选中的模板 */
const selectedTemplate = computed<TaskTemplate | null>(
  () => templateList.value.find((item) => item.id === form.template_id) ?? null,
);

/** `t-select` 不吃 `null` → `null ↔ undefined` 代理 */
const templateProxy = computed<string | undefined>({
  get: () => form.template_id ?? undefined,
  set: (value) => {
    form.template_id = typeof value === 'string' && value ? value : null;
  },
});

/** 拉模板下拉（新建态；模板数量少，一次取 200 条） */
async function loadTemplates(): Promise<void> {
  templateLoading.value = true;
  try {
    const result = await listTaskTemplates();
    templateList.value = result.list;
    templateOptions.value = result.list.map((item) => ({
      label: item.enabled ? `${item.name}（周期：${item.cron_expr}）` : item.name,
      value: item.id,
    }));
  } catch {
    templateOptions.value = [];
  } finally {
    templateLoading.value = false;
  }
}

/** 选中模板 → 回填可见字段（子任务与落库由后端按模板做） */
function handleTemplateChange(value: unknown): void {
  if (typeof value !== 'string' || !value) {
    return;
  }
  const hit = templateList.value.find((item) => item.id === value);
  if (!hit) {
    return;
  }
  form.title = hit.title;
  form.description = hit.description ?? '';
  form.priority = hit.priority;
  form.need_review = hit.need_review;
  form.estimate_hours = hit.estimate_hours;
  if (hit.project_id) {
    form.project_id = hit.project_id;
  }
}

/** 是否为编辑态 */
const isEdit = computed(() => Boolean(props.taskId));

/**
 * 预估工时双向代理。
 *
 * `t-input-number` 的 modelValue 类型是 `InputNumberValue | undefined`（不吃 `null`），
 * 而表单模型用 `null` 表示「未估」—— 这里做一次 `null ↔ undefined` 转换，
 * 避免为了迁就组件把「未估」写成 0（0 小时后端会当成「估了 0 小时」）。
 */
const estimateHoursProxy = computed<number | undefined>({
  get: () => form.estimate_hours ?? undefined,
  set: (value) => {
    form.estimate_hours = typeof value === 'number' && !Number.isNaN(value) ? value : null;
  },
});

/**
 * 所属项目双向代理。
 *
 * `t-select` 的 modelValue 不吃 `null`（类型里没有），而表单模型用 `null` 表示
 * 「不归入项目」—— 这里做一次 `null ↔ undefined` 转换（与预估工时的代理同款）。
 */
const projectProxy = computed<string | undefined>({
  get: () => form.project_id ?? undefined,
  set: (value) => {
    form.project_id = typeof value === 'string' && value ? value : null;
  },
});

/** 对话框可见性代理（对外 v-model） */
const innerVisible = computed({
  get: () => props.visible,
  set: (value: boolean) => emit('update:visible', value),
});

/**
 * 截止日期校验：**新建时**不得早于今天（编辑时允许补录历史任务的旧截止日，
 * 否则存量逾期任务一改就报错）。按自然日比较，忽略时分秒。
 *
 * 「不得晚于父任务截止日」只做**提示**不阻断：后端的 `63112` 校验挂在
 * `PATCH /task/{id}/schedule`（改期）上，创建 / 编辑接口并不拦，
 * 前端若硬拦会挡掉后端本来允许的创建。
 */
function validateDueDate(value: unknown): boolean | { result: boolean; message: string } {
  if (!value || isEdit.value) {
    return true;
  }
  const target = dayjs(String(value)).startOf('day');
  if (target.isBefore(dayjs().startOf('day'))) {
    return { result: false, message: '截止日期不能早于今天' };
  }
  return true;
}

/** 预估工时校验：有值时必须 > 0（0 小时的任务没有意义，留空表示未估） */
function validateHours(value: unknown): boolean | { result: boolean; message: string } {
  if (value === null || value === undefined || value === '') {
    return true;
  }
  const hours = Number(value);
  if (Number.isNaN(hours) || hours <= 0) {
    return { result: false, message: '预估工时必须大于 0' };
  }
  if (hours > 9999) {
    return { result: false, message: '预估工时不能超过 9999 小时' };
  }
  return true;
}

/** 校验规则 */
const rules = {
  title: [
    { required: true, message: '请填写任务标题', trigger: 'blur' as const },
    { max: 200, message: '标题不能超过 200 字', trigger: 'blur' as const },
  ],
  due_date: [{ validator: validateDueDate, trigger: 'change' as const }],
  estimate_hours: [{ validator: validateHours, trigger: 'change' as const }],
};

/** 用详情回填表单（编辑态） */
async function loadDetail(id: string): Promise<void> {
  detailLoading.value = true;
  try {
    const detail = await getTask(id);
    form.title = detail.title;
    form.description = detail.description ?? '';
    form.priority = detail.priority;
    form.owner_id = detail.owner_id;
    form.due_date = detail.due_date ?? '';
    form.estimate_hours = detail.estimate_hours;
    form.need_review = detail.need_review;
    form.project_id = detail.project_id;
    // 父任务只读回显（编辑不改父级）
    form.parent_id = detail.parent_id;
    editParentNo.value = detail.parent_task_no ?? '';
  } catch (error) {
    const message = error instanceof ApiError ? error.message : '任务详情加载失败';
    MessagePlugin.error(message);
    innerVisible.value = false;
  } finally {
    detailLoading.value = false;
  }
}

/** 打开时初始化：编辑 → 拉详情回填；新建 → 清空 */
watch(
  () => props.visible,
  (visible) => {
    if (!visible) {
      return;
    }
    Object.assign(form, emptyForm());
    editParentNo.value = '';
    formRef.value?.clearValidate?.();
    void loadProjectOptions();
    void loadTemplates();
    // 编辑态也要拉：父任务是只读展示，「编号 标题」得从候选里取（详情只给 parent_task_no）
    void loadParentOptions();
    if (props.taskId) {
      void loadDetail(props.taskId);
    }
  },
);

/** 提交 */
async function handleSubmit(): Promise<void> {
  const result = await formRef.value?.validate?.();
  if (result !== true) {
    return;
  }
  saving.value = true;
  try {
    const description = form.description.trim();
    if (props.taskId) {
      // 编辑：显式 null 才算「清空」，缺席 = 保持原值（后端 double Option 语义）
      const payload: TaskUpdateReq = {
        title: form.title.trim(),
        description: description ? description : null,
        priority: form.priority,
        need_review: form.need_review,
        due_date: form.due_date || null,
        estimate_hours: form.estimate_hours ?? null,
        // 所属项目：显式传 null = 移出项目（后端 double Option 语义）
        project_id: form.project_id || null,
      };
      if (form.owner_id) {
        payload.owner_id = form.owner_id;
      }
      const saved = await updateTask(props.taskId, payload);
      MessagePlugin.success('任务已更新');
      emit('saved', saved.id);
    } else if (form.template_id) {
      // E11：按模板建任务（子任务由后端一次落库；标题/负责人/项目/截止日可覆盖）
      const created = await createTaskFromTemplate(form.template_id, {
        title: form.title.trim(),
        ...(form.owner_id ? { owner_id: form.owner_id } : {}),
        ...(form.project_id ? { project_id: form.project_id } : {}),
        ...(form.due_date ? { due_date: form.due_date } : {}),
      });
      MessagePlugin.success(
        `已按模板创建：${created.task_no}（含 ${created.subtask_ids.length} 条子任务）`,
      );
      emit('saved', created.id);
    } else {
      const payload: TaskCreateReq = {
        title: form.title.trim(),
        priority: form.priority,
        need_review: form.need_review,
      };
      if (description) {
        payload.description = description;
      }
      if (form.owner_id) {
        payload.owner_id = form.owner_id;
      }
      if (form.due_date) {
        payload.due_date = form.due_date;
      }
      if (form.estimate_hours !== null) {
        payload.estimate_hours = form.estimate_hours;
      }
      if (form.project_id) {
        payload.project_id = form.project_id;
      }
      if (form.parent_id) {
        payload.parent_id = form.parent_id;
      }
      const saved = await createTask(payload);
      MessagePlugin.success(`任务已创建：${saved.task_no}`);
      emit('saved', saved.id);
    }
    innerVisible.value = false;
  } catch (error) {
    const message = error instanceof ApiError ? error.message : '保存失败，请稍后重试';
    MessagePlugin.error(message);
  } finally {
    saving.value = false;
  }
}

/** 关闭后重置脏数据，避免下次打开闪现上一次的内容 */
function handleClosed(): void {
  Object.assign(form, emptyForm());
  editParentNo.value = '';
  formRef.value?.clearValidate?.();
}
</script>

<style scoped lang="less">
.task-form {
  :deep(.t-form__item) {
    margin-bottom: 16px;
  }
}

.form-hint {
  margin-left: 8px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);

  &--warn {
    color: var(--td-warning-color);
  }
}

.form-readonly {
  display: inline-block;
  font-size: 13px;
  color: var(--td-text-color-secondary);
}
</style>

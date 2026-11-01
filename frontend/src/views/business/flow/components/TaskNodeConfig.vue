<template>
  <!--
    「任务配置」面板（设计 §4.1 的 ★ 块，仅 TASK_NODE 显示）。

    组件的根是**片段**（没有包裹层）：这些 `<t-form-item>` 要落在 `NodeProps.vue` 的
    `<t-form>` 里（那个 form 带着 `label-width` 与 `disabled=readonly`），
    自己再套一层 `<t-form>` 会让标签宽度和只读态各算一套。
  -->
  <div class="task-node-config__head">
    <span class="task-node-config__title">
      <t-icon name="task" />
      任务配置
    </span>
    <t-tag size="small" variant="light" theme="primary">节点激活即生成任务</t-tag>
  </div>

  <!-- ---------- 标题 / 描述（模板 + 变量插入） ---------- -->
  <t-form-item label="任务标题" required-mark>
    <div class="task-node-config__field" data-var-field="title" @focusin="remember('title', $event)">
      <t-textarea
        :value="cfg.title_template"
        placeholder="如：{{instance.biz_no}} 催货：{{form.supplier_name}}"
        :maxlength="MAX_TITLE_CHARS"
        :autosize="{ minRows: 1, maxRows: 3 }"
        :disabled="readonly"
        :data-testid="'task-title-template'"
        @change="(value: unknown) => patch({ title_template: String(value ?? '') })"
      />
      <div class="task-node-config__field-bar">
        <TemplateVarPicker
          :biz-type="bizType"
          :form-fields="formFields"
          :readonly="readonly"
          @insert="(path: string) => insertVar('title', path)"
        />
        <span class="task-node-config__count">{{ (cfg.title_template ?? '').length }} / {{ MAX_TITLE_CHARS }}</span>
      </div>
    </div>
    <ul v-if="titleIssues.length > 0" class="task-node-config__issues" data-testid="task-title-issues">
      <li v-for="(issue, index) in titleIssues" :key="index">
        <t-tag size="small" variant="light" theme="danger">{{ issue.code }}</t-tag>
        {{ issue.reason }}
      </li>
    </ul>
    <p class="task-node-config__tip">必填。支持变量：点「变量」插到光标处，选中文字则替换选中段。</p>
  </t-form-item>

  <t-form-item label="任务描述">
    <div class="task-node-config__field" data-var-field="desc" @focusin="remember('desc', $event)">
      <t-textarea
        :value="cfg.description_template ?? ''"
        placeholder="选填。如：采购单 {{instance.biz_no}} 已审批，请于 {{node.name}} 前催到货"
        :maxlength="MAX_DESCRIPTION_CHARS"
        :autosize="{ minRows: 2, maxRows: 6 }"
        :disabled="readonly"
        @change="(value: unknown) => patch({ description_template: String(value ?? '') || null })"
      />
      <div class="task-node-config__field-bar">
        <TemplateVarPicker
          :biz-type="bizType"
          :form-fields="formFields"
          :readonly="readonly"
          @insert="(path: string) => insertVar('desc', path)"
        />
        <span class="task-node-config__count">
          {{ (cfg.description_template ?? '').length }} / {{ MAX_DESCRIPTION_CHARS }}
        </span>
      </div>
    </div>
    <ul v-if="descIssues.length > 0" class="task-node-config__issues" data-testid="task-desc-issues">
      <li v-for="(issue, index) in descIssues" :key="index">
        <t-tag size="small" variant="light" theme="danger">{{ issue.code }}</t-tag>
        {{ issue.reason }}
      </li>
    </ul>
  </t-form-item>

  <!-- ---------- 优先级 / 截止时间 ---------- -->
  <t-form-item label="优先级">
    <t-select
      :value="cfg.priority"
      :options="priorityOptions"
      :disabled="readonly"
      @change="(value: unknown) => patch({ priority: String(value ?? 'P2') })"
    />
  </t-form-item>

  <t-form-item label="截止时间">
    <div class="task-node-config__due">
      <t-radio-group
        :value="cfg.due?.mode ?? 'NONE'"
        :disabled="readonly"
        @change="(value: unknown) => onDueMode(String(value ?? 'NONE'))"
      >
        <t-radio value="NONE">不设期限</t-radio>
        <t-radio v-for="item in DUE_MODE_OPTIONS" :key="item.value" :value="item.value">{{ item.label }}</t-radio>
      </t-radio-group>

      <div v-if="cfg.due?.mode === 'RELATIVE'" class="task-node-config__due-row">
        <t-input-number
          :value="cfg.due?.amount ?? undefined"
          :min="1"
          :max="3650"
          theme="normal"
          :disabled="readonly"
          @change="(value: unknown) => patchDue({ amount: toNumber(value) })"
        />
        <t-select
          :value="cfg.due?.unit ?? 'WORKDAY'"
          :options="unitOptions"
          :disabled="readonly"
          class="task-node-config__due-unit"
          @change="(value: unknown) => patchDue({ unit: String(value ?? 'WORKDAY') as DueUnit })"
        />
        <span class="task-node-config__due-hint">从节点激活时刻起算（工作日跳过周六周日）</span>
      </div>

      <div v-else-if="cfg.due?.mode === 'ABSOLUTE'" class="task-node-config__due-row">
        <t-date-picker
          :value="cfg.due?.date ?? undefined"
          format="YYYY-MM-DD"
          value-type="YYYY-MM-DD"
          :disabled="readonly"
          @change="(value: unknown) => patchDue({ date: String(value ?? '') || null })"
        />
        <span class="task-node-config__due-hint">固定日期；运行期若已过期，会落到当天并记 WARN</span>
      </div>

      <div v-else-if="cfg.due?.mode === 'FIELD'" class="task-node-config__due-row">
        <t-input
          :value="cfg.due?.field ?? ''"
          placeholder="form.delivery_date"
          :maxlength="72"
          :disabled="readonly"
          @change="(value: unknown) => patchDue({ field: String(value ?? '') || null })"
        />
        <span class="task-node-config__due-hint">只能取表单字段（必须以 form. 开头）</span>
      </div>

      <p v-if="dueIssueText" class="task-node-config__issue-text">{{ dueIssueText }}</p>
    </div>
  </t-form-item>

  <!-- ---------- R18：完成方式 ⇄ 需验收（双向联动） ---------- -->
  <t-form-item label="完成方式">
    <div class="task-node-config__complete">
      <t-select
        :value="cfg.on_complete"
        :options="onCompleteSelectOptions"
        :disabled="readonly"
        data-testid="task-on-complete"
        @change="(value: unknown) => onCompleteChange(String(value ?? 'DIRECT'))"
      />
      <p class="task-node-config__tip">{{ onCompleteHint }}</p>

      <t-tooltip :content="reviewTooltip" placement="top-left">
        <span class="task-node-config__review" :class="{ 'is-locked': reviewLocked }">
          <t-checkbox
            :checked="cfg.need_review"
            :disabled="readonly || reviewLocked"
            data-testid="task-need-review"
            @change="(value: unknown) => onNeedReviewChange(Boolean(value))"
          >
            完成后需验收
          </t-checkbox>
        </span>
      </t-tooltip>
      <p v-if="reviewLocked && !readonly" class="task-node-config__tip task-node-config__tip--warn">
        {{ reviewTooltip }}
      </p>

      <!-- on_complete=APPROVE 才出现的「审批业务类型」 -->
      <div v-if="cfg.on_complete === 'APPROVE'" class="task-node-config__approve">
        <t-select
          :value="cfg.approve_biz_type ?? undefined"
          :options="approveBizTypeOptions"
          :loading="approveLoading"
          placeholder="选择要发起的审批流程类型（必填）"
          :disabled="readonly"
          data-testid="task-approve-biz-type"
          @change="(value: unknown) => patch({ approve_biz_type: String(value ?? '') || null })"
        />
        <!-- 必填校验：只在没选时红字；选了值只给中性提示，绝不因为「选了 APPROVE」就报警 -->
        <p v-if="approveIssue" class="task-node-config__issue-text" data-testid="task-approve-biz-type-issue">
          {{ approveIssue }}
        </p>
        <p v-else class="task-node-config__tip" data-testid="task-approve-biz-type-tip">
          发布时会校验：该业务类型下必须有一条「已发布」且「设为默认」的流程，否则发布会返回 65035 —— 先发布那条审批流再来选。
        </p>
        <p v-if="approveBizTypeOptions.length === 0 && !approveLoading" class="task-node-config__issue-text">
          暂无可用审批流程（需要一条 `已发布` 且 `is_default=true` 的流程定义）。
        </p>
      </div>

      <p v-if="matrixIssue" class="task-node-config__issue-text" data-testid="task-matrix-issue">{{ matrixIssue }}</p>
    </div>
  </t-form-item>

  <!-- ---------- 打回 / 取消 / 展示 ---------- -->
  <t-form-item label="打回时">
    <t-select
      :value="cfg.on_reject"
      :options="onRejectOptions"
      :disabled="readonly"
      @change="(value: unknown) => patch({ on_reject: String(value ?? 'BACK_TO_PREV') as OnReject })"
    />
    <p class="task-node-config__tip">验收被驳回或任务被打回（待验收 → 进行中）时流程怎么走。</p>
  </t-form-item>

  <t-form-item label="取消时">
    <t-select
      :value="cfg.on_cancel"
      :options="onCancelOptions"
      :disabled="readonly"
      @change="(value: unknown) => patch({ on_cancel: String(value ?? 'TERMINATE') as OnCancel })"
    />
    <p class="task-node-config__tip">活被取消了，流程不该继续往下走（默认终止流程）。</p>
  </t-form-item>

  <t-form-item label="详情页显示所属单据">
    <t-switch
      :value="cfg.attach_biz"
      :disabled="readonly"
      @change="(value: unknown) => patch({ attach_biz: Boolean(value) })"
    />
  </t-form-item>

  <!-- ---------- L1 实时预演 ---------- -->
  <TaskPreview
    :config="cfg"
    :biz-type="bizType"
    :node-name="node?.name ?? null"
    :assignee="previewAssignee"
  />
</template>

<script setup lang="ts">
/**
 * 任务配置面板（`TASK_NODE`）。
 *
 * # 它只做三件事
 *
 * 1. 把 `node.config.task` 反解成面板状态（{@link taskConfigOf}），改一个字段就抛
 *    `patch({ config: { task } })`（**只带 config 一个键**，撤销栈一步一条）；
 * 2. **R18 双向联动**：下拉 / 勾选框的 `@change` 只调
 *    {@link applyOnComplete} / {@link applyNeedReview} —— 判据在 `task-node-config.ts`（可单测），
 *    组件里不写任何 `if (onComplete === ...)` 的业务判断；
 * 3. 变量插入：记住最后聚焦的输入框，用它的 `selectionStart/End` 插 `{{path}}`，
 *    插完把光标放到 `}}` 之后（选中文字则替换选中段）。
 *
 * # 为什么插入位置要自己管
 *
 * `t-textarea` 的 `v-model` 是受控的：点弹层里的变量会**先让输入框失焦**，
 * 失焦后 `selectionStart` 归零 —— 直接读会得到 0，插到最前面。
 * 所以在 `focusin` 时就把 `<textarea>` 元素存下来，插入时用它自己记住的选区。
 *
 * # 术语文案（决策 4，红线）
 *
 * 验收 = `need_review`（UI 上只出现「验收」）；审批 = `on_complete=APPROVE`。
 * 两个词**不许互换**，也不许写成「审核」「复核」「确认」。
 */
import { computed, nextTick, ref, watch } from 'vue';
import {
  pageFlowDefs,
  type FlowNode,
  type TaskPreviewAssigneeReq,
} from '@/api/business/flow-api';
import TemplateVarPicker from '@/views/business/flow/components/TemplateVarPicker.vue';
import TaskPreview from '@/views/business/flow/components/TaskPreview.vue';
import { validateTemplateVars, insertPlaceholder, type VarIssue } from '@/views/business/flow/components/template-vars';
import {
  DUE_MODE_OPTIONS,
  DUE_UNIT_OPTIONS,
  ON_CANCEL_OPTIONS,
  ON_COMPLETE_OPTIONS,
  ON_REJECT_OPTIONS,
  TASK_PRIORITY_OPTIONS,
  applyNeedReview,
  applyOnComplete,
  approveBizTypeIssue,
  approveOptionDisabled,
  checkMatrix,
  dueIssue,
  needReviewLocked,
  needReviewTooltip,
  taskConfigOf,
  taskConfigPatch,
  type OnCancel,
  type OnComplete,
  type OnReject,
  type TaskConfig,
  type TaskDue,
  type DueMode,
  type DueUnit,
} from '@/views/business/flow/components/task-node-config';

/** 标题模板字符上限（`node_config.rs::MAX_TITLE_TEMPLATE_CHARS`） */
const MAX_TITLE_CHARS = 200;
/** 描述模板字符上限（`node_config.rs::MAX_DESCRIPTION_TEMPLATE_CHARS`） */
const MAX_DESCRIPTION_CHARS = 8000;

/** 组件入参 */
const props = withDefaults(
  defineProps<{
    /** 选中节点（`type` 必须是 `TASK_NODE`，由 `NodeProps` 保证） */
    node?: FlowNode | null;
    /** 本流程业务类型（决定 `task.*` 变量 + 指派来源灰化） */
    bizType?: string | null;
    /** 表单字段候选（来自历史实例的 `form_data`，可能为空） */
    formFields?: string[];
    /** 只读（已发布版本） */
    readonly?: boolean;
  }>(),
  { node: null, bizType: null, formFields: () => [], readonly: false },
);

/** 组件事件：只抛 `config`（`patch-node` 的载荷由父组件转发） */
const emit = defineEmits<{
  /** 改节点属性 */
  (event: 'patch', payload: Partial<FlowNode>): void;
}>();

/** 面板状态（**从 props 派生**，不持有自己的副本 —— 撤销回退时自动跟上） */
const cfg = computed<TaskConfig>(() => taskConfigOf(props.node));

/** 优先级 / 打回 / 取消 / 期限单位选项（纯常量，来自 task-node-config.ts） */
const priorityOptions = TASK_PRIORITY_OPTIONS.map((item) => ({ label: item.label, value: item.value }));
const unitOptions = DUE_UNIT_OPTIONS.map((item) => ({ label: item.label, value: item.value }));
const onRejectOptions = ON_REJECT_OPTIONS.map((item) => ({ label: item.label, value: item.value }));
const onCancelOptions = ON_CANCEL_OPTIONS.map((item) => ({ label: item.label, value: item.value }));

/** 完成方式下拉：勾了验收就把 APPROVE 灰掉（第 2 条纪律） */
const onCompleteSelectOptions = computed(() =>
  ON_COMPLETE_OPTIONS.map((item) => ({
    label: item.label,
    value: item.value,
    disabled: item.value === 'APPROVE' && approveOptionDisabled(cfg.value),
  })),
);

/** 当前完成方式的人话解释 */
const onCompleteHint = computed(
  () => ON_COMPLETE_OPTIONS.find((item) => item.value === cfg.value.on_complete)?.hint ?? '',
);

/** 验收勾选框是否锁定 */
const reviewLocked = computed(() => needReviewLocked(cfg.value));

/** 验收勾选框的说明（锁定 / 未锁定两种文案） */
const reviewTooltip = computed(() => needReviewTooltip(cfg.value));

/** R18 矩阵问题（正常操作永远 null；脏数据才可能非空） */
const matrixIssue = computed(() => checkMatrix(cfg.value));

/** `APPROVE` 的必填校验（没选审批流 biz_type 才提示；选了值不报警） */
const approveIssue = computed(() => approveBizTypeIssue(cfg.value));

/** 期限配置问题 */
const dueIssueText = computed(() => dueIssue(cfg.value.due));

/** 标题模板的变量问题 */
const titleIssues = computed<VarIssue[]>(() => validateTemplateVars(cfg.value.title_template ?? '', props.bizType));

/** 描述模板的变量问题 */
const descIssues = computed<VarIssue[]>(() =>
  validateTemplateVars(cfg.value.description_template ?? '', props.bizType),
);

/** 抛一个 `config.task` 补丁 */
function patch(next: Partial<TaskConfig>): void {
  if (props.readonly) {
    return;
  }
  emit('patch', taskConfigPatch({ ...cfg.value, ...next }));
}

/** 改期限（整块替换，避免 RELATIVE 的 amount 残留在 ABSOLUTE 上被后端 65034 拒） */
function patchDue(next: Partial<TaskDue>): void {
  const base = cfg.value.due ?? { mode: 'RELATIVE' as DueMode };
  patch({ due: { ...base, ...next } });
}

/** 切期限模式：只保留该模式需要的键 */
function onDueMode(mode: string): void {
  if (mode === 'NONE') {
    patch({ due: null });
    return;
  }
  if (mode === 'RELATIVE') {
    patch({ due: { mode: 'RELATIVE', amount: cfg.value.due?.amount ?? 3, unit: cfg.value.due?.unit ?? 'WORKDAY' } });
    return;
  }
  if (mode === 'ABSOLUTE') {
    patch({ due: { mode: 'ABSOLUTE', date: cfg.value.due?.date ?? null } });
    return;
  }
  patch({ due: { mode: 'FIELD', field: cfg.value.due?.field ?? null } });
}

/** 完成方式变化 → R18 联动 */
function onCompleteChange(value: string): void {
  const next = applyOnComplete(cfg.value, value as OnComplete);
  patch({
    on_complete: next.on_complete,
    need_review: next.need_review,
    approve_biz_type: next.approve_biz_type,
  });
  if (next.on_complete === 'APPROVE') {
    void loadApproveBizTypes();
  }
}

/** 需验收变化 → R18 反向联动 */
function onNeedReviewChange(checked: boolean): void {
  const next = applyNeedReview(cfg.value, checked);
  patch({
    need_review: next.need_review,
    on_complete: next.on_complete,
    approve_biz_type: next.approve_biz_type,
  });
}

/** 「可选数字输入框」→ number | null */
function toNumber(value: unknown): number | null {
  if (value === undefined || value === null || value === '') {
    return null;
  }
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

// ==================== 审批业务类型候选（APPROVE 才加载） ====================

/** 候选（`已发布` 且 `is_default=true` 的流程类型） */
const approveBizTypeOptions = ref<Array<{ label: string; value: string }>>([]);
/** 加载中 */
const approveLoading = ref(false);
/** 是否已加载过（一次就够） */
let approveLoaded = false;

/**
 * 拉「可发起的审批流程类型」。
 *
 * 没有 `form-fields` 那样的专用接口，所以这里用分页接口 + **前端按 `is_default` 过滤**：
 * 只列真实存在的 PUBLISHED 默认流程，**不伪造候选**（下拉里出现一个发不起来的类型，
 * 比下拉是空的更糟）。空列表时组件显示「暂无可用审批流程」。
 */
async function loadApproveBizTypes(): Promise<void> {
  if (approveLoaded || approveLoading.value) {
    return;
  }
  approveLoading.value = true;
  try {
    const page = await pageFlowDefs({ status: 'PUBLISHED', page_size: 100 });
    const picked = (page.list ?? []).filter((item) => item.is_default);
    const seen = new Set<string>();
    approveBizTypeOptions.value = picked
      .filter((item) => {
        if (seen.has(item.biz_type)) {
          return false;
        }
        seen.add(item.biz_type);
        return true;
      })
      .map((item) => ({ label: `${item.biz_type}（${item.flow_name}）`, value: item.biz_type }));
    approveLoaded = true;
  } catch {
    // 拿不到候选就留空并显示「暂无」—— 不阻断面板其他编辑（下次切到 APPROVE 会重试）
    approveBizTypeOptions.value = [];
  } finally {
    approveLoading.value = false;
  }
}

// ==================== 变量插入（光标位置） ====================

/** 最后一次聚焦的字段 */
const lastField = ref<'title' | 'desc'>('title');
/** 最后一次聚焦的输入框元素（失焦后仍可用它自己的选区） */
let lastElement: HTMLTextAreaElement | null = null;
/** 记下聚焦的输入框（`focusin` 会从内部 textarea 冒泡上来） */
function remember(field: 'title' | 'desc', event: FocusEvent): void {
  const target = event.target as HTMLElement | null;
  if (target instanceof HTMLTextAreaElement) {
    lastField.value = field;
    lastElement = target;
  }
}

/** 按字段名找输入框（记住的元素被卸载时的兜底） */
function elementOf(field: 'title' | 'desc'): HTMLTextAreaElement | null {
  // 右侧属性面板同一时刻只挂一份 `TASK_NODE` 面板，按 `data-var-field` 全局查是唯一的
  return document.querySelector<HTMLTextAreaElement>(`[data-var-field="${field}"] textarea`);
}

/**
 * 在指定字段的光标处插入变量。
 *
 * 三条口径（卡面验收 3）：光标在中间 → 插在光标处且光标落 `}}` 之后；
 * 有选中文字 → 替换选中段；拿不到选区 → 插到末尾（不抛错）。
 */
function insertVar(field: 'title' | 'desc', path: string): void {
  if (props.readonly) {
    return;
  }
  const current = field === 'title' ? cfg.value.title_template ?? '' : cfg.value.description_template ?? '';
  const element = lastField.value === field ? lastElement : null;
  const start = element?.selectionStart ?? current.length;
  const end = element?.selectionEnd ?? start;
  const { text, caret } = insertPlaceholder(current, path, start, end);
  if (field === 'title') {
    patch({ title_template: text });
  } else {
    patch({ description_template: text || null });
  }
  void nextTick(() => {
    const target = element ?? elementOf(field);
    if (!target) {
      return;
    }
    target.focus();
    target.setSelectionRange(caret, caret);
    // 组件被复用（切节点）时元素可能已换，兜底再找一次
    if (target !== elementOf(field)) {
      const fresh = elementOf(field);
      fresh?.focus();
      fresh?.setSelectionRange(caret, caret);
    }
  });
}

// ==================== 预演用的指派配置 ====================

/**
 * 预演用的指派配置：把节点上**能编辑的几项**翻译成 `PreviewAssigneeReq`。
 *
 * 节点没配 `assignee_type` 时给 `null`（预演只回模板结果，负责人那行显示「未配置」）。
 */
const previewAssignee = computed<TaskPreviewAssigneeReq | null>(() => {
  const node = props.node;
  if (!node?.assignee_type) {
    return null;
  }
  const refIds = (node.assignees ?? []).map((item) => item.ref_id).filter((id) => Boolean(id));
  return {
    assignee_type: node.assignee_type,
    role_codes: node.assignee_type === 'ROLE' ? refIds : [],
    user_ids: node.assignee_type === 'USER' ? refIds : [],
    superior_levels: node.superior_levels ?? 1,
    form_var_key: node.form_var_key ?? null,
    empty_assignee: node.empty_assignee ?? 'ERROR',
    allow_robot: Boolean(node.allow_robot),
  };
});

/** 切到 APPROVE（含撤销回退到 APPROVE）时补拉候选 */
watch(
  () => cfg.value.on_complete,
  (value) => {
    if (value === 'APPROVE') {
      void loadApproveBizTypes();
    }
  },
  { immediate: true },
);
</script>

<style scoped lang="less">
.task-node-config__head {
  display: flex;
  gap: 8px;
  align-items: center;
  justify-content: space-between;
  padding: 8px 0 4px;
  margin: 6px 0 4px;
  border-top: 1px dashed var(--td-component-stroke);
}

.task-node-config__title {
  display: inline-flex;
  gap: 6px;
  align-items: center;
  font-size: 13px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.task-node-config__field {
  width: 100%;
}

.task-node-config__field-bar {
  display: flex;
  gap: 8px;
  align-items: center;
  justify-content: space-between;
  margin-top: 4px;
}

.task-node-config__count {
  font-size: 11px;
  color: var(--td-text-color-placeholder);
}

.task-node-config__issues {
  padding-left: 16px;
  margin: 6px 0 0;
  font-size: 12px;
  line-height: 1.6;
  color: var(--td-error-color);
}

.task-node-config__issue-text {
  margin: 4px 0 0;
  font-size: 12px;
  line-height: 1.5;
  color: var(--td-error-color);
}

.task-node-config__tip {
  margin: 4px 0 0;
  font-size: 12px;
  line-height: 1.5;
  color: var(--td-text-color-placeholder);

  &--warn {
    color: var(--td-warning-color);
  }
}

.task-node-config__due,
.task-node-config__complete {
  display: flex;
  flex-direction: column;
  gap: 6px;
  width: 100%;
}

.task-node-config__due-row {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  align-items: center;
}

.task-node-config__due-unit {
  width: 108px;
}

.task-node-config__due-hint {
  font-size: 11px;
  color: var(--td-text-color-placeholder);
}

.task-node-config__review {
  display: inline-flex;
  width: fit-content;

  &.is-locked {
    opacity: 0.75;
  }
}

.task-node-config__approve {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
</style>

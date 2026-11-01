<template>
  <!--
    「通知节点配置」面板（P-FLOW-2 · 设计 §2.6）。

    组件的根是**片段**（没有包裹层）：这些 `<t-form-item>` 要落在 `NodeProps.vue` 的
    `<t-form>` 里（那个 form 带着 `label-width` 与 `disabled=readonly`），
    自己再套一层 `<t-form>` 会让标签宽度和只读态各算一套。
  -->
  <div class="notify-node-config__head">
    <span class="notify-node-config__title">
      <t-icon name="notification" />
      通知节点配置
    </span>
    <t-tag size="small" variant="light" theme="success">发完即通过，不阻塞流程</t-tag>
  </div>

  <!-- ---------- 时机 ---------- -->
  <t-form-item label="发送时机">
    <t-radio-group
      :value="cfg.when"
      variant="default-filled"
      :disabled="readonly"
      @change="(value: unknown) => patch({ when: value as NotifyWhen })"
    >
      <t-radio-button v-for="item in WHEN_OPTIONS" :key="item.value" :value="item.value">
        {{ item.label }}
      </t-radio-button>
    </t-radio-group>
    <p class="notify-node-config__tip">{{ whenHint }}</p>
  </t-form-item>

  <!-- ---------- 收件人（1–5 个来源） ---------- -->
  <t-form-item label="通知给谁" required-mark>
    <div class="notify-node-config__targets" data-testid="notify-targets">
      <div v-for="(target, index) in cfg.targets" :key="`${target.type}-${index}`" class="notify-node-config__target">
        <div class="notify-node-config__target-head">
          <span class="notify-node-config__target-index">{{ index + 1 }}</span>
          <t-select
            :value="target.type"
            :options="targetTypeOptions"
            class="notify-node-config__target-type"
            :disabled="readonly"
            @change="(value: unknown) => onTargetType(index, value as NotifyTargetType)"
          />
          <t-button
            v-if="cfg.targets.length > 1"
            size="small"
            variant="text"
            theme="danger"
            :disabled="readonly"
            @click="removeTarget(index)"
          >
            <t-icon name="delete" />
          </t-button>
        </div>

        <!-- USER -->
        <UserSelect
          v-if="target.type === 'USER'"
          :model-value="target.user_ids[0] ?? ''"
          :multiple="true"
          class="notify-node-config__target-value"
          @update:model-value="(value: string | string[] | null) => onUsers(index, value)"
        />
        <!-- ROLE：用 **role_code**（后端按 code 查，跨环境稳定） -->
        <t-select
          v-else-if="target.type === 'ROLE'"
          :value="target.role_codes[0] ?? undefined"
          :options="roleOptions"
          :loading="roleLoading"
          filterable
          multiple
          placeholder="选择角色"
          class="notify-node-config__target-value"
          :disabled="readonly"
          @change="(value: unknown) => onRoles(index, value)"
        />
        <!-- DEPT_LEADER -->
        <DeptTreeSelect
          v-else-if="target.type === 'DEPT_LEADER'"
          :model-value="target.dept_ids[0] ?? ''"
          class="notify-node-config__target-value"
          @update:model-value="(value: string | undefined) => onDepts(index, value)"
        />
        <!-- INITIATOR_SUPERIOR -->
        <div v-else-if="target.type === 'INITIATOR_SUPERIOR'" class="notify-node-config__inline">
          <span>上溯</span>
          <t-input-number
            :value="target.levels"
            :min="1"
            :max="10"
            theme="normal"
            :disabled="readonly"
            @change="(value: unknown) => onLevels(index, Number(value) || 1)"
          />
          <span>层主管</span>
        </div>
        <!-- FORM_VAR -->
        <t-input
          v-else-if="target.type === 'FORM_VAR'"
          :value="target.form_var_key ?? ''"
          placeholder="表单字段名，如 buyer_id"
          class="notify-node-config__target-value"
          :disabled="readonly"
          @change="(value: unknown) => onFormVar(index, String(value ?? ''))"
        />
        <p v-else class="notify-node-config__tip">{{ targetTypeHint(target.type) }}</p>

        <p v-if="targetIssueText(target)" class="notify-node-config__issue">
          {{ targetIssueText(target) }}
        </p>
      </div>

      <t-button
        size="small"
        variant="outline"
        :disabled="readonly || cfg.targets.length >= MAX_NOTIFY_TARGETS"
        @click="addTarget"
      >
        <template #icon><t-icon name="add" /></template>
        添加收件人（{{ cfg.targets.length }} / {{ MAX_NOTIFY_TARGETS }}）
      </t-button>
    </div>
  </t-form-item>

  <!-- ---------- 模板二选一 ---------- -->
  <t-form-item label="通知内容" required-mark>
    <div class="notify-node-config__template">
      <t-radio-group
        :value="cfg.template_mode"
        variant="default-filled"
        :disabled="readonly"
        @change="(value: unknown) => onTemplateMode(value as NotifyTemplateMode)"
      >
        <t-radio-button value="inline">自己写</t-radio-button>
        <t-radio-button value="code">用已有模板</t-radio-button>
      </t-radio-group>

      <!-- 引用已有模板 -->
      <template v-if="cfg.template_mode === 'code'">
        <t-select
          :value="cfg.template_code ?? undefined"
          :options="templateOptions"
          :loading="templateLoading"
          filterable
          placeholder="选择通知模板（系统 → 通知模板）"
          data-testid="notify-template-code"
          :disabled="readonly"
          @change="(value: unknown) => patch({ template_code: String(value ?? '') || null })"
        />
        <p class="notify-node-config__tip">
          模板里的变量名是**无点号**写法（<code v-pre>{{instance_title}}</code>），由通知模板管理维护；
          改文案不用发版，但发布前会校验模板存在且启用。
        </p>
        <TemplateVarPicker mode="template-ref" :readonly="true" />
      </template>

      <!-- 内联 -->
      <template v-else>
        <div class="notify-node-config__field" data-var-field="title" @focusin="remember('title', $event)">
          <t-textarea
            :value="cfg.inline_title"
            placeholder="如：{{instance.title}} 已到「{{node.name}}」"
            :maxlength="MAX_NOTIFY_TITLE_CHARS"
            :autosize="{ minRows: 1, maxRows: 3 }"
            data-testid="notify-inline-title"
            :disabled="readonly"
            @change="(value: unknown) => patch({ inline_title: String(value ?? '') })"
          />
          <div class="notify-node-config__field-bar">
            <TemplateVarPicker
              :biz-type="bizType"
              :form-fields="formFields"
              :readonly="readonly"
              @insert="(path: string) => insertVar('title', path)"
            />
            <span class="notify-node-config__count">{{ cfg.inline_title.length }} / {{ MAX_NOTIFY_TITLE_CHARS }}</span>
          </div>
        </div>
        <div class="notify-node-config__field" data-var-field="content" @focusin="remember('content', $event)">
          <t-textarea
            :value="cfg.inline_content"
            placeholder="如：单号 {{instance.biz_no}}｜发起人 {{initiator.name}}"
            :maxlength="MAX_NOTIFY_CONTENT_CHARS"
            :autosize="{ minRows: 2, maxRows: 6 }"
            data-testid="notify-inline-content"
            :disabled="readonly"
            @change="(value: unknown) => patch({ inline_content: String(value ?? '') })"
          />
          <div class="notify-node-config__field-bar">
            <TemplateVarPicker
              :biz-type="bizType"
              :form-fields="formFields"
              :readonly="readonly"
              @insert="(path: string) => insertVar('content', path)"
            />
            <span class="notify-node-config__count">{{ cfg.inline_content.length }} / {{ MAX_NOTIFY_CONTENT_CHARS }}</span>
          </div>
        </div>
        <ul v-if="varIssues.length > 0" class="notify-node-config__issues" data-testid="notify-var-issues">
          <li v-for="(issue, index) in varIssues" :key="index">
            <t-tag size="small" variant="light" theme="danger">{{ issue.code }}</t-tag>
            {{ issue.reason }}
          </li>
        </ul>
        <t-button size="small" variant="text" :disabled="readonly" @click="previewOpen = !previewOpen">
          <template #icon><t-icon :name="previewOpen ? 'chevron-up' : 'browse'" /></template>
          {{ previewOpen ? '收起预览' : '预览（样例数据）' }}
        </t-button>
        <div v-if="previewOpen" class="notify-node-config__preview" data-testid="notify-preview">
          <div class="notify-node-config__preview-label">样例预览（用示例数据渲染，实际以运行期数据为准）</div>
          <div class="notify-node-config__preview-title">{{ previewTitle }}</div>
          <div class="notify-node-config__preview-content">{{ previewContent }}</div>
        </div>
      </template>
    </div>
  </t-form-item>

  <!-- ---------- 级别 / 通道 / 送达 ---------- -->
  <t-form-item label="级别与通道">
    <div class="notify-node-config__inline">
      <t-select
        :value="cfg.level"
        :options="LEVEL_OPTIONS"
        class="notify-node-config__half"
        :disabled="readonly"
        @change="(value: unknown) => patch({ level: value as NotifyLevel })"
      />
      <t-select
        :value="cfg.channels"
        :options="CHANNEL_OPTIONS"
        multiple
        class="notify-node-config__half"
        :disabled="readonly"
        @change="(value: unknown) => onChannels(value)"
      />
    </div>
    <t-checkbox
      :checked="cfg.force"
      :disabled="readonly"
      @change="(value: unknown) => patch({ force: value === true })"
    >
      强制送达（跳过收件人的「站内信」偏好）
    </t-checkbox>
    <p class="notify-node-config__tip">
      默认尊重偏好：流程通知多数是「知情」不是「必办」。要「必须让人知道」才勾。
    </p>
  </t-form-item>

  <t-form-item label="无人可发时">
    <t-select
      :value="cfg.skip_if_empty"
      :options="skipOptions"
      :disabled="readonly"
      @change="(value: unknown) => patch({ skip_if_empty: value as SkipIfEmpty })"
    />
    <p class="notify-node-config__tip">{{ skipHint }}</p>
  </t-form-item>

  <!-- ---------- 校正后的问题清单（正常操作应该永远看不到） ---------- -->
  <t-form-item v-if="issues.length > 0" label="需要修正">
    <ul class="notify-node-config__issues" data-testid="notify-issues">
      <li v-for="(issue, index) in issues" :key="index">{{ issue }}</li>
    </ul>
  </t-form-item>
</template>

<script setup lang="ts">
/**
 * 通知节点配置面板（`NOTIFY_NODE`，P-FLOW-2）。
 *
 * # 三件事
 *
 * 1. 把 `node.config.notify` 反解成面板状态（{@link notifyConfigOf}），改一个字段就抛
 *    `patch({ config: { notify } })`（**只带 config 一个键**，撤销栈一步一条）；
 * 2. 判据全部复用 `notify-node-config.ts` 的纯函数（{@link notifyIssues} /
 *    {@link targetIssue}），组件里不写业务判断 —— 与后端 `parse_notify_config` 对齐，
 *    免得两边各写一套然后漂移；
 * 3. 变量插入复用 **P-FLOW-1C 的 `TemplateVarPicker`**（同一个选择器、同一套白名单），
 *    插入行为（记住焦点输入框的选区、替换选中段）也与任务配置面板一致。
 *
 * # 预览为什么是「样例」而不是「真渲染」
 *
 * 真渲染在引擎事务里做（需要实例 / 表单 / 上一审批人，缺变量是 400）。配置阶段这些都
 * 还不存在，所以这里用一份**标注清楚的样例数据**渲染，让用户看到「模板长什么样」。
 * UI 上写明「样例预览」，不假装那是真结果（{@link renderSample}）。
 */
import { computed, nextTick, ref } from 'vue';
import type { FlowNode } from '@/api/business/flow-api';
import { listRoles } from '@/api/system/role-api';
import { pageNoticeTemplates } from '@/api/business/notice';
import UserSelect from '@/views/business/task/components/UserSelect.vue';
import DeptTreeSelect from '@/components/DeptTreeSelect.vue';
import TemplateVarPicker from '@/views/business/flow/components/TemplateVarPicker.vue';
import { insertPlaceholder, validateTemplateVars, type VarIssue } from '@/views/business/flow/components/template-vars';
import {
  CHANNEL_OPTIONS,
  LEVEL_OPTIONS,
  MAX_NOTIFY_CONTENT_CHARS,
  MAX_NOTIFY_TARGETS,
  MAX_NOTIFY_TITLE_CHARS,
  SKIP_OPTIONS,
  TARGET_TYPE_OPTIONS,
  WHEN_OPTIONS,
  emptyTarget,
  notifyConfigOf,
  notifyConfigPatch,
  notifyIssues,
  renderSample,
  targetIssue,
  targetTypeLabel,
  type NotifyChannel,
  type NotifyConfig,
  type NotifyLevel,
  type NotifyTargetType,
  type NotifyTemplateMode,
  type NotifyWhen,
  type SkipIfEmpty,
} from '@/views/business/flow/components/notify-node-config';

/** 组件入参 */
const props = withDefaults(
  defineProps<{
    /** 选中节点（`type` 必须是 `NOTIFY_NODE`，由 `NodeProps` 保证） */
    node?: FlowNode | null;
    /** 本流程业务类型（模板变量白名单用） */
    bizType?: string | null;
    /** 表单字段候选 */
    formFields?: string[];
    /** 只读（已发布版本） */
    readonly?: boolean;
  }>(),
  { node: null, bizType: null, formFields: () => [], readonly: false },
);

/** 组件事件：只抛 `config` */
const emit = defineEmits<{
  (event: 'patch', payload: Partial<FlowNode>): void;
}>();

/** 面板状态（**从 props 派生**，撤销回退时自动跟上） */
const cfg = computed<NotifyConfig>(() => notifyConfigOf(props.node));

/** 下拉选项（纯常量） */
const targetTypeOptions = TARGET_TYPE_OPTIONS.map((item) => ({ label: item.label, value: item.value }));
const skipOptions = SKIP_OPTIONS.map((item) => ({ label: item.label, value: item.value }));

/** 当前时机的人话解释 */
const whenHint = computed(() => WHEN_OPTIONS.find((item) => item.value === cfg.value.when)?.hint ?? '');

/** 「无人可发时」的人话解释 */
const skipHint = computed(
  () => SKIP_OPTIONS.find((item) => item.value === cfg.value.skip_if_empty)?.hint ?? '',
);

/** 整块配置的问题（正常操作应该永远为空） */
const issues = computed(() => notifyIssues(cfg.value));

/** 内联模板的变量问题（与任务配置面板同一套白名单判据） */
const varIssues = computed<VarIssue[]>(() => [
  ...validateTemplateVars(cfg.value.inline_title, props.bizType),
  ...validateTemplateVars(cfg.value.inline_content, props.bizType),
]);

/** 预览开关 */
const previewOpen = ref(false);
const previewTitle = computed(() => renderSample(cfg.value.inline_title));
const previewContent = computed(() => renderSample(cfg.value.inline_content));

/** 角色选项（值是 **role_code**，后端按 code 解析） */
const roleOptions = ref<Array<{ label: string; value: string }>>([]);
const roleLoading = ref(false);
let roleLoaded = false;

/** 模板选项（只列流程类模板 + 全部启用中的模板） */
const templateOptions = ref<Array<{ label: string; value: string }>>([]);
const templateLoading = ref(false);
let templateLoaded = false;

/** 抛一个 `config.notify` 补丁 */
function patch(next: Partial<NotifyConfig>): void {
  if (props.readonly) {
    return;
  }
  emit('patch', notifyConfigPatch({ ...cfg.value, ...next }));
}

/** 整块替换 targets（增删改都走它，避免「改了副本没回写」） */
function patchTargets(targets: NotifyConfig['targets']): void {
  patch({ targets });
}

function addTarget(): void {
  if (cfg.value.targets.length >= MAX_NOTIFY_TARGETS) {
    return;
  }
  patchTargets([...cfg.value.targets, emptyTarget()]);
}

function removeTarget(index: number): void {
  patchTargets(cfg.value.targets.filter((_item, position) => position !== index));
}

/** 换来源类型时清掉别的来源的键（后端对「用错键」是 65034） */
function onTargetType(index: number, type: NotifyTargetType): void {
  const next = cfg.value.targets.map((item, position) =>
    position === index ? { ...emptyTarget(type), levels: item.levels } : item,
  );
  patchTargets(next);
}

function onUsers(index: number, value: string | string[] | null): void {
  const list = Array.isArray(value) ? value : value ? [value] : [];
  patchTargets(
    cfg.value.targets.map((item, position) => (position === index ? { ...item, user_ids: list } : item)),
  );
}

function onRoles(index: number, value: unknown): void {
  const list = Array.isArray(value) ? value.map((item) => String(item ?? '')) : [];
  patchTargets(
    cfg.value.targets.map((item, position) =>
      position === index ? { ...item, role_codes: list.filter((code) => code !== '') } : item,
    ),
  );
}

function onDepts(index: number, value: string | undefined): void {
  const list = value ? [value] : [];
  patchTargets(
    cfg.value.targets.map((item, position) => (position === index ? { ...item, dept_ids: list } : item)),
  );
}

function onLevels(index: number, levels: number): void {
  patchTargets(
    cfg.value.targets.map((item, position) => (position === index ? { ...item, levels } : item)),
  );
}

function onFormVar(index: number, key: string): void {
  patchTargets(
    cfg.value.targets.map((item, position) =>
      position === index ? { ...item, form_var_key: key || null } : item,
    ),
  );
}

function onChannels(value: unknown): void {
  const list = (Array.isArray(value) ? value : []).map((item) => String(item ?? '')) as NotifyChannel[];
  patch({ channels: list.length > 0 ? list : ['INBOX'] });
}

/** 切模板模式：切到哪边就把另一边的键清掉（后端对「两个都给」是 65034） */
function onTemplateMode(mode: NotifyTemplateMode): void {
  if (mode === 'code') {
    void loadTemplates();
    patch({ template_mode: 'code' });
    return;
  }
  patch({ template_mode: 'inline', template_code: null });
}

/** 收件人来源的说明（不需要额外键的类型才有提示） */
function targetTypeHint(type: NotifyTargetType): string {
  switch (type) {
    case 'INITIATOR':
      return '发给流程发起人本人。';
    case 'TASK_OWNER':
    case 'TASK_CREATOR':
    case 'TASK_ASSIGNER':
      return `「${targetTypeLabel(type)}」只在「任务验收」（biz_type=TASK_ACCEPT）的流程里有值，其它流程解析出 0 人。`;
    case 'ADMIN':
      return '发给平台超级管理员。';
    default:
      return '';
  }
}

/** 单个收件人的必填提示 */
function targetIssueText(target: NotifyConfig['targets'][number]): string {
  return targetIssue(target) ?? '';
}

// ---------- 变量插入（与任务配置面板同款：记住最后聚焦的输入框） ----------

type FieldKey = 'title' | 'content';

const activeField = ref<FieldKey>('title');
const elements = new Map<FieldKey, HTMLTextAreaElement>();

function remember(field: FieldKey, event: FocusEvent): void {
  const element = event.target as HTMLElement | null;
  if (element instanceof HTMLTextAreaElement) {
    elements.set(field, element);
  }
  activeField.value = field;
}

async function insertVar(field: FieldKey, path: string): Promise<void> {
  const store = elements.get(field);
  const current = field === 'title' ? cfg.value.inline_title : cfg.value.inline_content;
  const start = store?.selectionStart ?? current.length;
  const end = store?.selectionEnd ?? current.length;
  const next = insertPlaceholder(current, path, start, end);
  patch(field === 'title' ? { inline_title: next.text } : { inline_content: next.text });
  await nextTick();
  const element = elements.get(field);
  if (element) {
    element.focus();
    element.setSelectionRange(next.caret, next.caret);
  }
}

// ---------- 远程数据（都是懒加载，失败静默降级为空列表） ----------

async function loadRoles(): Promise<void> {
  if (roleLoaded || roleLoading.value) {
    return;
  }
  roleLoading.value = true;
  try {
    const list = await listRoles();
    // 值是 **role_code**：后端 `targets[].role_codes` 按 code 查，跨环境稳定
    roleOptions.value = list.map((role) => ({ label: role.role_name, value: role.role_code }));
    roleLoaded = true;
  } catch {
    roleOptions.value = [];
  } finally {
    roleLoading.value = false;
  }
}

async function loadTemplates(): Promise<void> {
  if (templateLoaded || templateLoading.value) {
    return;
  }
  templateLoading.value = true;
  try {
    const page = await pageNoticeTemplates({ page: 1, page_size: 100 });
    templateOptions.value = (page.list ?? [])
      .filter((item) => item.status === 1)
      .map((item) => ({ label: `${item.template_name}（${item.template_code}）`, value: item.template_code }));
    templateLoaded = true;
  } catch {
    templateOptions.value = [];
  } finally {
    templateLoading.value = false;
  }
}

// 打开面板就把两个下拉喂满（代价很小，且用户随时可能切来源 / 切模板模式）
void loadRoles();
void loadTemplates();
</script>

<style scoped lang="less">
.notify-node-config {
  &__head {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 4px 0 12px;
    font-weight: 600;
  }

  &__title {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  &__tip {
    margin: 4px 0 0;
    font-size: 12px;
    line-height: 1.6;
    color: var(--td-text-color-placeholder);
  }

  &__targets {
    display: flex;
    flex-direction: column;
    gap: 10px;
    width: 100%;
  }

  &__target {
    padding: 8px;
    border: 1px solid var(--td-component-stroke);
    border-radius: 4px;
    background: var(--td-bg-color-container);
  }

  &__target-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  &__target-index {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    font-size: 12px;
    color: #fff;
    background: var(--td-brand-color);
    border-radius: 50%;
  }

  &__target-type {
    flex: 1;
  }

  &__target-value {
    margin-top: 6px;
    width: 100%;
  }

  &__inline {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  &__half {
    flex: 1;
  }

  &__issue {
    margin: 6px 0 0;
    font-size: 12px;
    color: var(--td-error-color);
  }

  &__template {
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: 100%;
  }

  &__field {
    display: flex;
    flex-direction: column;
  }

  &__field-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }

  &__count {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__issues {
    margin: 4px 0 0;
    padding-left: 18px;
    font-size: 12px;
    line-height: 1.7;
    color: var(--td-error-color);
  }

  &__preview {
    padding: 10px;
    border: 1px dashed var(--td-component-stroke);
    border-radius: 4px;
    background: var(--td-bg-color-secondarycontainer);
  }

  &__preview-label {
    margin-bottom: 6px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__preview-title {
    font-weight: 600;
  }

  &__preview-content {
    margin-top: 4px;
    font-size: 12px;
    line-height: 1.7;
    white-space: pre-wrap;
  }
}
</style>

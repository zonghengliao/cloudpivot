<template>
  <div class="notice-send-page">
    <t-card :bordered="false" class="send-card">
      <div class="send-card__head">
        <div>
          <h3 class="send-card__title">发送通知</h3>
          <p class="send-card__desc">站内信为系统基础通道，发送后立即进入接收人的消息中心（E-NTC-10）。</p>
        </div>
        <t-button theme="default" @click="goBack">返回</t-button>
      </div>

      <t-form :data="form" label-width="110px" class="send-form">
        <t-form-item label="通知类型">
          <t-select v-model="form.notice_type" class="send-form__field">
            <t-option v-for="item in TYPE_OPTIONS" :key="item.value" :value="item.value" :label="item.label" />
          </t-select>
        </t-form-item>
        <t-form-item label="级别">
          <t-radio-group v-model="form.level">
            <t-radio value="INFO">通知</t-radio>
            <t-radio value="SUCCESS">成功</t-radio>
            <t-radio value="WARN">警告</t-radio>
            <t-radio value="ERROR">错误</t-radio>
          </t-radio-group>
        </t-form-item>
        <t-form-item label="使用模板">
          <t-select
            v-model="form.template_code"
            class="send-form__field"
            clearable
            placeholder="选填：选择模板后按模板渲染"
            @change="onTemplateChange"
          >
            <t-option v-for="item in templates" :key="item.template_code" :value="item.template_code" :label="item.template_name" />
          </t-select>
        </t-form-item>

        <template v-if="selectedTemplate">
          <t-form-item v-for="variable in selectedTemplate.variables" :key="variable.name" :label="variable.name">
            <t-input v-model="templateVars[variable.name]" class="send-form__field" :placeholder="variable.label || '变量值'" />
          </t-form-item>
          <t-alert
            v-if="missingVars.length"
            class="send-form__alert"
            theme="warning"
            :message="`变量 ${missingVars.join('、')} 未定义，请补齐（E-NTC-01）`"
          />
        </template>

        <template v-else>
          <t-form-item label="标题">
            <t-input v-model="form.title" class="send-form__field" :maxlength="200" placeholder="通知标题" />
            <div v-if="titleTooLong" class="field-hint field-hint--error">
              标题不能超过 200 个字符（当前 {{ form.title.length }}）
            </div>
          </t-form-item>
          <t-form-item label="正文">
            <t-textarea
              v-model="form.content"
              class="send-form__field"
              :autosize="{ minRows: 5, maxRows: 10 }"
              placeholder="通知正文，可换行"
            />
            <div v-if="contentTooLong" class="field-hint field-hint--error">
              正文不能超过 5000 个字符（当前 {{ form.content.length }}）
            </div>
          </t-form-item>
        </template>

        <t-form-item label="跳转地址">
          <t-input v-model="form.link_url" class="send-form__field" placeholder="选填，如 /business/impexp/import-task" />
        </t-form-item>

        <t-form-item label="发送对象">
          <t-radio-group v-model="form.target_type" @change="onTargetTypeChange">
            <t-radio value="USER">指定用户</t-radio>
            <t-radio value="ROLE">按角色</t-radio>
            <t-radio value="DEPT">按部门（含下级）</t-radio>
            <t-radio value="ALL">全员</t-radio>
          </t-radio-group>
        </t-form-item>

        <t-form-item v-if="form.target_type === 'USER'" label="选择用户">
          <t-select
            v-model="form.target_ids"
            class="send-form__field"
            multiple
            filterable
            clearable
            :options="userOptions"
            :loading="userLoading"
            placeholder="搜索用户名 / 姓名后选择"
            @search="searchUsers"
          />
          <div class="field-hint">指定用户发送上限 500 人（E-NTC-05）</div>
        </t-form-item>

        <t-form-item v-else-if="form.target_type === 'ROLE'" label="选择角色">
          <t-select v-model="form.target_ids" class="send-form__field" multiple clearable :options="roleOptions" placeholder="选择角色" />
        </t-form-item>

        <t-form-item v-else-if="form.target_type === 'DEPT'" label="选择部门">
          <t-tree-select
            v-model="form.target_ids"
            class="send-form__field"
            :data="deptTree"
            :keys="{ value: 'id', label: 'dept_name', children: 'children' }"
            multiple
            clearable
            placeholder="选择部门（自动包含下级）"
          />
        </t-form-item>

        <t-form-item v-else label="范围">
          <t-tag theme="warning" variant="light">全员发送将命中系统内全部启用用户，请谨慎操作</t-tag>
        </t-form-item>

        <t-form-item label="发送时间">
          <t-radio-group v-model="sendMode">
            <t-radio value="now">立即发送</t-radio>
            <t-radio value="schedule">定时发送</t-radio>
          </t-radio-group>
        </t-form-item>
        <t-form-item v-if="sendMode === 'schedule'" label="定时时间">
          <t-date-picker
            v-model="scheduledAt"
            class="send-form__field"
            enable-time-picker
            format="YYYY-MM-DD HH:mm"
            placeholder="选择发送时间（需晚于当前时间 5 分钟）"
          />
          <div v-if="scheduleTooSoon" class="field-hint field-hint--error">
            定时发送时间必须晚于当前时间 5 分钟以上（E-NTC-07）
          </div>
          <div class="field-hint field-hint--warn">
            当前后端 dispatch 尚无定时字段，定时发送暂不可提交（已记录为后端缺陷）。
          </div>
        </t-form-item>

        <t-form-item v-if="previewVisible" label="预览">
          <div class="send-preview">
            <div class="send-preview__title">{{ previewTitle }}</div>
            <div class="send-preview__content">{{ previewContent }}</div>
          </div>
        </t-form-item>
      </t-form>

      <div class="send-footer">
        <t-button variant="outline" :disabled="!selectedTemplate" @click="handlePreview">预览渲染</t-button>
        <span class="send-footer__spacer" />
        <t-button theme="default" @click="goBack">取消</t-button>
        <t-button v-privilege="[PERM.noticeSend]" theme="primary" :loading="submitting" @click="confirmSend">发送</t-button>
      </div>
    </t-card>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import dayjs from 'dayjs';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import { previewNoticeTemplate, pageNoticeTemplates, sendNotice, type NoticeTemplateRes } from '@/api/business/notice';
import { pageUsers, type UserRes } from '@/api/system/user-api';
import { listRoles } from '@/api/system/role-api';
import { getDeptTree, type DeptNode } from '@/api/system/dept-api';
import { PERM } from '@/constants/permissions';

const router = useRouter();
const route = useRoute();

const TYPE_OPTIONS = [
  { value: 'SYSTEM', label: '系统消息' },
  { value: 'TODO', label: '待办' },
  { value: 'ALERT', label: '告警' },
  { value: 'WORKFLOW', label: '审批' },
  { value: 'CIRCULAR', label: '公告' },
];

const form = reactive({
  notice_type: 'SYSTEM',
  level: 'INFO',
  title: '',
  content: '',
  link_url: '',
  template_code: '',
  target_type: 'USER',
  target_ids: [] as string[],
});
const templateVars = reactive<Record<string, string>>({});
const sendMode = ref<'now' | 'schedule'>('now');
const scheduledAt = ref('');
const submitting = ref(false);
const previewVisible = ref(false);
const previewTitle = ref('');
const previewContent = ref('');

const templates = ref<NoticeTemplateRes[]>([]);
const userOptions = ref<Array<{ label: string; value: string }>>([]);
const userLoading = ref(false);
const roleOptions = ref<Array<{ label: string; value: string }>>([]);
const deptTree = ref<DeptNode[]>([]);

const selectedTemplate = computed(() => templates.value.find((item) => item.template_code === form.template_code));

const titleTooLong = computed(() => form.title.length > 200);
const contentTooLong = computed(() => form.content.length > 5000);

/** 模板声明的必填变量是否都填了（E-NTC-01） */
const missingVars = computed(() => {
  if (!selectedTemplate.value) {
    return [];
  }
  return (selectedTemplate.value.variables ?? [])
    .filter((variable) => variable.required && !(templateVars[variable.name] || '').trim())
    .map((variable) => variable.name);
});

/** 定时时间必须晚于当前 5 分钟（E-NTC-07） */
const scheduleTooSoon = computed(() => {
  if (sendMode.value !== 'schedule' || !scheduledAt.value) {
    return false;
  }
  return dayjs(scheduledAt.value).isBefore(dayjs().add(5, 'minute'));
});

async function loadTemplates(): Promise<void> {
  try {
    const data = await pageNoticeTemplates({ page: 1, page_size: 100, status: 1 });
    templates.value = data.list;
    const preset = route.query.template;
    if (typeof preset === 'string' && preset) {
      form.template_code = preset;
      onTemplateChange();
    }
  } catch {
    // 无模板权限时静默：用户仍然可以手写标题正文发送
  }
}

async function loadRoles(): Promise<void> {
  try {
    const list = await listRoles();
    roleOptions.value = list.map((role) => ({ label: role.role_name, value: role.id }));
  } catch {
    // 忽略：角色列表只在切到「按角色」时才需要
  }
}

async function loadDepts(): Promise<void> {
  try {
    deptTree.value = await getDeptTree();
  } catch {
    // 忽略：部门树只在切到「按部门」时才需要
  }
}

async function searchUsers(keyword: string): Promise<void> {
  userLoading.value = true;
  try {
    const data = await pageUsers({ page: 1, page_size: 20, username: keyword || undefined });
    userOptions.value = data.list.map((user: UserRes) => ({
      label: `${user.real_name || user.username}（${user.username}）`,
      value: user.id,
    }));
  } catch {
    userOptions.value = [];
  } finally {
    userLoading.value = false;
  }
}

function onTemplateChange(): void {
  previewVisible.value = false;
  const template = selectedTemplate.value;
  if (!template) {
    return;
  }
  form.notice_type = (template.notice_type || 'SYSTEM').toUpperCase();
  form.level = (template.default_level || 'INFO').toUpperCase();
  for (const variable of template.variables ?? []) {
    if (!(variable.name in templateVars)) {
      templateVars[variable.name] = variable.sample ?? '';
    }
  }
}

function onTargetTypeChange(value: unknown): void {
  form.target_ids = [];
  if (value === 'ROLE' && roleOptions.value.length === 0) {
    void loadRoles();
  }
  if (value === 'DEPT' && deptTree.value.length === 0) {
    void loadDepts();
  }
  if (value === 'USER' && userOptions.value.length === 0) {
    void searchUsers('');
  }
}

async function handlePreview(): Promise<void> {
  if (!selectedTemplate.value) {
    return;
  }
  try {
    const vars: Record<string, string> = {};
    for (const variable of selectedTemplate.value.variables ?? []) {
      vars[variable.name] = templateVars[variable.name] ?? '';
    }
    const res = await previewNoticeTemplate(selectedTemplate.value.id, vars);
    previewTitle.value = res.title;
    previewContent.value = res.content;
    previewVisible.value = true;
  } catch (error) {
    showError(error);
  }
}

/** 提交前校验：目标非空、上限、定时规则、模板变量齐全 */
function validate(): string {
  if (form.target_type !== 'ALL' && form.target_ids.length === 0) {
    return '发送对象为空，请至少选择 1 个用户 / 角色 / 部门（E-NTC-06）';
  }
  if (form.target_type === 'USER' && form.target_ids.length > 500) {
    return '指定用户发送上限 500 人，如需更大范围请选「部门」或「全员」（E-NTC-05）';
  }
  if (selectedTemplate.value) {
    if (missingVars.value.length > 0) {
      return `变量 ${missingVars.value.join('、')} 未定义（E-NTC-01）`;
    }
  } else {
    if (!form.title.trim()) {
      return '请填写通知标题';
    }
    if (!form.content.trim()) {
      return '请填写通知正文';
    }
  }
  if (titleTooLong.value || contentTooLong.value) {
    return '标题或正文超出长度限制（E-NTC-03 / E-NTC-04）';
  }
  if (sendMode.value === 'schedule') {
    if (!scheduledAt.value) {
      return '请选择定时发送时间';
    }
    if (scheduleTooSoon.value) {
      return '定时发送时间必须晚于当前时间 5 分钟以上（E-NTC-07）';
    }
    return 'SYSTEM_SCHEDULE_UNSUPPORTED';
  }
  return '';
}

function confirmSend(): void {
  const message = validate();
  if (message === 'SYSTEM_SCHEDULE_UNSUPPORTED') {
    void MessagePlugin.warning('当前后端尚未支持定时发送（dispatch 无 scheduled_at 字段），请选择「立即发送」');
    return;
  }
  if (message) {
    void MessagePlugin.warning(message);
    return;
  }
  const countText = form.target_type === 'ALL' ? '全部启用用户' : `${form.target_ids.length} 个目标`;
  const dialog = DialogPlugin.confirm({
    header: '确认发送',
    body: `即将向 ${countText} 发送通知，不可撤回。标题：${form.title || selectedTemplate.value?.template_name || form.template_code}。是否继续？（E-NTC-12）`,
    confirmBtn: { content: '发送', theme: 'primary' },
    onConfirm: () => {
      dialog.destroy();
      void doSend();
    },
  });
}

async function doSend(): Promise<void> {
  submitting.value = true;
  try {
    const vars: Record<string, string> = {};
    for (const key of Object.keys(templateVars)) {
      vars[key] = templateVars[key];
    }
    const res = await sendNotice({
      notice_type: form.notice_type,
      level: form.level,
      title: selectedTemplate.value ? undefined : form.title.trim(),
      content: selectedTemplate.value ? undefined : form.content.trim(),
      link_url: form.link_url || undefined,
      template_code: form.template_code || undefined,
      template_vars: selectedTemplate.value ? vars : undefined,
      target_type: form.target_type,
      target_ids: form.target_type === 'ALL' ? [] : form.target_ids,
      idempotency_key: `manual-${Date.now()}`,
    });
    void MessagePlugin.success(`已发送 ${res.receiver_count} 人`);
    goBack();
  } catch (error) {
    showError(error);
  } finally {
    submitting.value = false;
  }
}

function goBack(): void {
  void router.push('/business/notice/template');
}

function showError(error: unknown): void {
  void MessagePlugin.error(error instanceof ApiError ? error.message : '发送失败，请稍后重试');
}

onMounted(() => {
  void loadTemplates();
  void searchUsers('');
});
</script>

<style lang="less" scoped>
.notice-send-page {
  padding: 16px;

  .send-card {
    &__head {
      display: flex;
      align-items: flex-start;
      justify-content: space-between;
      padding-bottom: 12px;
      border-bottom: 1px solid var(--td-component-stroke);
    }

    &__title {
      margin: 0 0 4px;
      font-size: 16px;
      color: var(--td-text-color-primary);
    }

    &__desc {
      margin: 0;
      font-size: 12px;
      color: var(--td-text-color-secondary);
    }
  }

  .send-form {
    max-width: 780px;
    margin-top: 16px;

    &__field {
      width: 420px;
    }

    &__alert {
      margin: 0 0 16px 110px;
    }
  }

  .send-preview {
    width: 420px;
    padding: 12px;
    border: 1px solid var(--td-component-stroke);
    border-radius: 6px;
    background: var(--td-bg-color-page);

    &__title {
      margin-bottom: 8px;
      font-weight: 600;
    }

    &__content {
      font-size: 13px;
      line-height: 1.7;
      color: var(--td-text-color-secondary);
      white-space: pre-wrap;
    }
  }

  .field-hint {
    margin-top: 4px;
    font-size: 12px;
    line-height: 1.6;
    color: var(--td-text-color-secondary);

    &--error {
      color: var(--td-error-color);
    }

    &--warn {
      color: var(--td-warning-color);
    }
  }

  .send-footer {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-top: 16px;
    padding-top: 16px;
    border-top: 1px solid var(--td-component-stroke);

    &__spacer {
      flex: 1;
    }
  }
}
</style>

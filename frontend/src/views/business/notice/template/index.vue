<template>
  <div class="notice-template-page">
    <t-card :bordered="false" class="filter-card">
      <t-row :gutter="16">
        <t-col :span="4">
          <t-input v-model="query.keyword" placeholder="模板名称 / 编码" clearable @enter="handleSearch" />
        </t-col>
        <t-col :span="3">
          <t-select v-model="query.notice_type" placeholder="通知类型" clearable>
            <t-option v-for="item in TYPE_OPTIONS" :key="item.value" :value="item.value" :label="item.label" />
          </t-select>
        </t-col>
        <t-col :span="3">
          <t-select v-model="query.status" placeholder="状态" clearable>
            <t-option :value="1" label="启用" />
            <t-option :value="0" label="停用" />
          </t-select>
        </t-col>
        <t-col :span="3">
          <t-space>
            <t-button theme="primary" @click="handleSearch">
              <template #icon><t-icon name="search" /></template>
              查询
            </t-button>
            <t-button theme="default" @click="handleReset">重置</t-button>
          </t-space>
        </t-col>
      </t-row>
    </t-card>

    <t-card :bordered="false" class="table-card">
      <div class="toolbar">
        <t-space>
          <t-button v-privilege="[PERM.noticeTemplateCreate]" theme="primary" @click="openCreate">
            <template #icon><t-icon name="add" /></template>
            新增模板
          </t-button>
          <!-- E10：发送通知是隐藏菜单（/business/notice/send 不在侧边栏），这里用主色描边按钮把入口顶出来 -->
          <t-button v-privilege="[PERM.noticeSend]" theme="primary" variant="outline" @click="goSend">
            <template #icon><t-icon name="send" /></template>
            发送通知
          </t-button>
          <t-button theme="default" :loading="loading" @click="loadList">
            <template #icon><t-icon name="refresh" /></template>
            刷新
          </t-button>
        </t-space>
        <span class="toolbar__count">共 {{ pagination.total }} 条</span>
      </div>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        :stripe="true"
        @page-change="handlePageChange"
      >
        <template #template_name="{ row }">
          <span class="cell-name">{{ row.template_name }}</span>
          <span class="cell-sub">{{ row.template_code }}</span>
        </template>
        <template #notice_type="{ row }">{{ typeLabel(row.notice_type) }}</template>
        <template #default_level="{ row }">
          <t-tag :theme="levelTheme(row.default_level)" size="small" variant="light">{{ levelLabel(row.default_level) }}</t-tag>
        </template>
        <template #variables="{ row }">
          <t-space v-if="row.variables?.length" size="4" break-line>
            <t-tag v-for="variable in row.variables" :key="variable.name" size="small" variant="outline">
              {{ variable.name }}
            </t-tag>
          </t-space>
          <span v-else class="cell-empty">无变量</span>
        </template>
        <template #status="{ row }">
          <t-tag :theme="row.status === 1 ? 'success' : 'default'" size="small" variant="light">
            {{ row.status === 1 ? '启用' : '停用' }}
          </t-tag>
        </template>
        <template #updated_at="{ row }">{{ formatDateTime(row.updated_at) }}</template>
        <template #op="{ row }">
          <t-space size="small">
            <t-link v-privilege="[PERM.noticeTemplateUpdate]" theme="primary" @click="openEdit(row)">编辑</t-link>
            <t-link v-privilege="[PERM.noticeSend]" theme="primary" @click="goSendWith(row)">测试发送</t-link>
            <t-link v-privilege="[PERM.noticeTemplateDelete]" theme="danger" @click="confirmRemove(row)">删除</t-link>
          </t-space>
        </template>
        <template #empty>
          <div class="empty-state">
            <t-icon class="empty-state__icon" name="mail" />
            <p class="empty-state__title">还没有通知模板</p>
            <p class="empty-state__desc">模板用 {{ VAR_EXAMPLE }} 这类占位符声明变量，发送时按接收人渲染</p>
            <t-button v-privilege="[PERM.noticeTemplateCreate]" theme="primary" @click="openCreate">新增模板</t-button>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- 编辑抽屉：左表单 + 右变量面板 / 预览 -->
    <t-drawer
      v-model:visible="formVisible"
      :header="isEdit ? '编辑通知模板' : '新增通知模板'"
      size="860px"
      :footer="false"
      destroy-on-close
    >
      <div class="template-editor">
        <div class="template-editor__main">
          <t-form :data="form" label-width="96px">
            <t-form-item label="模板名称" required>
              <t-input v-model="form.template_name" :maxlength="64" placeholder="如：导入完成通知" />
            </t-form-item>
            <t-form-item label="模板编码" required>
              <t-input v-model="form.template_code" :disabled="isEdit" placeholder="大写字母/数字/下划线，如 NOTICE_IMPORT_DONE" />
            </t-form-item>
            <t-form-item label="通知类型">
              <t-select v-model="form.notice_type">
                <t-option v-for="item in TYPE_OPTIONS" :key="item.value" :value="item.value" :label="item.label" />
              </t-select>
            </t-form-item>
            <t-form-item label="默认级别">
              <t-select v-model="form.default_level">
                <t-option value="INFO" label="通知" />
                <t-option value="SUCCESS" label="成功" />
                <t-option value="WARN" label="警告" />
                <t-option value="ERROR" label="错误" />
              </t-select>
            </t-form-item>
            <t-form-item label="标题模板" required>
              <t-input v-model="form.title_template" :maxlength="200" placeholder="如：{{userName}} 的导入任务已完成" />
              <div v-if="titleTooLong" class="field-hint field-hint--error">
                标题不能超过 200 个字符（当前 {{ form.title_template.length }}）
              </div>
            </t-form-item>
            <t-form-item label="正文模板" required>
              <t-textarea v-model="form.content_template" :autosize="{ minRows: 6, maxRows: 12 }" placeholder="支持 {{变量}} 占位符" />
              <div v-if="contentTooLong" class="field-hint field-hint--error">
                正文不能超过 5000 个字符（当前 {{ form.content_template.length }}）
              </div>
              <div v-else class="field-hint">
                变量名不能含空格，请写作 <code>{{ VAR_EXAMPLE }}</code>（E-NTC-02）
              </div>
            </t-form-item>
            <t-form-item label="跳转地址">
              <t-input v-model="form.link_template" placeholder="选填，如 /business/impexp/import-task" />
            </t-form-item>
            <t-form-item label="状态">
              <t-radio-group v-model="form.status">
                <t-radio :value="1">启用</t-radio>
                <t-radio :value="0">停用</t-radio>
              </t-radio-group>
            </t-form-item>
          </t-form>
        </div>

        <div class="template-editor__side">
          <div class="side-block">
            <div class="side-block__title">检测到的变量</div>
            <div v-if="detectedVars.length === 0" class="field-hint">模板里还没有 {{ VAR_EXAMPLE }}</div>
            <div v-for="name in detectedVars" :key="name" class="var-row">
              <t-tag size="small" variant="light" theme="primary">{{ name }}</t-tag>
              <t-input v-model="sampleVars[name]" size="small" :placeholder="`${name} 的示例值`" />
            </div>
            <div v-if="unknownVars.length" class="field-hint field-hint--error">
              变量名不能含空格：{{ unknownVars.join('、') }}
            </div>
          </div>

          <div class="side-block">
            <div class="side-block__title">预览</div>
            <t-button size="small" variant="outline" :loading="previewing" :disabled="!isEdit || !canPreview" @click="handlePreview">
              试渲染
            </t-button>
            <div v-if="preview" class="preview-card">
              <div class="preview-card__title">{{ preview.title }}</div>
              <div class="preview-card__content">{{ preview.content }}</div>
            </div>
            <div v-else class="field-hint">{{ isEdit ? '填写示例值后点「试渲染」' : '保存后即可试渲染' }}</div>
          </div>
        </div>
      </div>

      <div class="editor-footer">
        <span class="editor-footer__spacer" />
        <t-button theme="default" @click="formVisible = false">取消</t-button>
        <t-button theme="primary" :loading="submitting" :disabled="titleTooLong || contentTooLong" @click="submitForm">
          保存
        </t-button>
      </div>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { useRouter } from 'vue-router';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import {
  createNoticeTemplate,
  deleteNoticeTemplate,
  pageNoticeTemplates,
  previewNoticeTemplate,
  updateNoticeTemplate,
  type NoticeTemplateRes,
  type TemplatePreviewRes,
} from '@/api/business/notice';
import { formatDateTime } from '@/utils/format';
import { PERM } from '@/constants/permissions';

/** 表格分页事件载荷 */
interface TablePageInfo {
  /** 当前页码 */
  current: number;
  /** 每页条数 */
  pageSize: number;
}

const router = useRouter();

/** 变量占位符示例（模板里直接写 `{{userName}}` 会被 Vue 当成插值，这里用常量绕开） */
const VAR_EXAMPLE = '{' + '{userName}' + '}'
const rows = ref<NoticeTemplateRes[]>([]);
const loading = ref(false);
const query = reactive({ keyword: '', notice_type: '' as string, status: undefined as number | undefined });
const pagination = reactive({ current: 1, pageSize: 10, total: 0 });

const TYPE_OPTIONS = [
  { value: 'SYSTEM', label: '系统消息' },
  { value: 'TODO', label: '待办' },
  { value: 'ALERT', label: '告警' },
  { value: 'WORKFLOW', label: '审批' },
  { value: 'CIRCULAR', label: '公告' },
];

function typeLabel(type: string): string {
  return TYPE_OPTIONS.find((item) => item.value === (type || '').toUpperCase())?.label ?? type;
}

function levelLabel(level: string): string {
  const map: Record<string, string> = { INFO: '通知', SUCCESS: '成功', WARN: '警告', ERROR: '错误' };
  return map[(level || '').toUpperCase()] || level;
}

function levelTheme(level: string): 'primary' | 'success' | 'warning' | 'danger' {
  const key = (level || '').toUpperCase();
  if (key === 'SUCCESS') {
    return 'success';
  }
  if (key === 'WARN') {
    return 'warning';
  }
  if (key === 'ERROR') {
    return 'danger';
  }
  return 'primary';
}

const columns: PrimaryTableCol[] = [
  { colKey: 'template_name', title: '模板', width: 240 },
  { colKey: 'notice_type', title: '类型', width: 110 },
  { colKey: 'default_level', title: '默认级别', width: 110 },
  { colKey: 'variables', title: '变量', width: 240, ellipsis: true },
  { colKey: 'status', title: '状态', width: 90 },
  { colKey: 'updated_at', title: '更新时间', width: 170 },
  { colKey: 'op', title: '操作', width: 200, fixed: 'right' },
];

async function loadList(): Promise<void> {
  loading.value = true;
  try {
    const data = await pageNoticeTemplates({
      page: pagination.current,
      page_size: pagination.pageSize,
      keyword: query.keyword || undefined,
      notice_type: query.notice_type || undefined,
      status: query.status,
    });
    rows.value = data.list;
    pagination.total = data.total;
  } catch (error) {
    showError(error);
  } finally {
    loading.value = false;
  }
}

function handleSearch(): void {
  pagination.current = 1;
  void loadList();
}

function handleReset(): void {
  query.keyword = '';
  query.notice_type = '';
  query.status = undefined;
  handleSearch();
}

function handlePageChange(pageInfo: TablePageInfo): void {
  pagination.current = pageInfo.current;
  pagination.pageSize = pageInfo.pageSize;
  void loadList();
}

// ==================== 编辑 ====================
const formVisible = ref(false);
const isEdit = ref(false);
const editingId = ref('');
const submitting = ref(false);
const previewing = ref(false);
const preview = ref<TemplatePreviewRes | null>(null);
const sampleVars = reactive<Record<string, string>>({});

const form = reactive({
  template_code: '',
  template_name: '',
  notice_type: 'SYSTEM',
  default_level: 'INFO',
  title_template: '',
  content_template: '',
  link_template: '',
  status: 1,
  content_format: 'TEXT',
  channels: 'inbox',
});

/** 标题超长（E-NTC-03） */
const titleTooLong = computed(() => form.title_template.length > 200);
/** 正文超长（E-NTC-04） */
const contentTooLong = computed(() => form.content_template.length > 5000);

/** 从模板中解析 `{{var}}`，同时挑出含空格的非法变量名（E-NTC-02） */
const detectedVars = computed<string[]>(() => {
  const names = new Set<string>();
  const regex = /\{\{\s*([^{}\s]+)\s*\}\}/g;
  const text = `${form.title_template}\n${form.content_template}`;
  let match = regex.exec(text);
  while (match) {
    names.add(match[1]);
    match = regex.exec(text);
  }
  return Array.from(names);
});

/** 含空格的变量名（如 `{{user Name}}`），保存前后端也会拒 */
const unknownVars = computed<string[]>(() => {
  const found: string[] = [];
  const regex = /\{\{\s*([^{}]*\s[^{}]*?)\s*\}\}/g;
  let match = regex.exec(`${form.title_template}\n${form.content_template}`);
  while (match) {
    found.push(match[1]);
    match = regex.exec(`${form.title_template}\n${form.content_template}`);
  }
  return found;
});

/** 每个变量都有示例值才能试渲染（后端渲染要求变量齐全，缺一个就 60018） */
const canPreview = computed(() => detectedVars.value.every((name) => (sampleVars[name] || '').trim() !== ''));

function resetVars(names: string[], defaults?: Record<string, string>): void {
  for (const key of Object.keys(sampleVars)) {
    if (!names.includes(key)) {
      delete sampleVars[key];
    }
  }
  for (const name of names) {
    if (!(name in sampleVars)) {
      sampleVars[name] = defaults?.[name] ?? '';
    }
  }
}

function openCreate(): void {
  isEdit.value = false;
  editingId.value = '';
  Object.assign(form, {
    template_code: '',
    template_name: '',
    notice_type: 'SYSTEM',
    default_level: 'INFO',
    title_template: '',
    content_template: '',
    link_template: '',
    status: 1,
    content_format: 'TEXT',
    channels: 'inbox',
  });
  preview.value = null;
  resetVars([]);
  formVisible.value = true;
}

function openEdit(row: NoticeTemplateRes): void {
  isEdit.value = true;
  editingId.value = row.id;
  Object.assign(form, {
    template_code: row.template_code,
    template_name: row.template_name,
    notice_type: (row.notice_type || 'SYSTEM').toUpperCase(),
    default_level: (row.default_level || 'INFO').toUpperCase(),
    title_template: row.title_template,
    content_template: row.content_template,
    link_template: row.link_template,
    status: row.status,
    content_format: row.content_format || 'TEXT',
    channels: row.channels || 'inbox',
  });
  preview.value = null;
  const defaults: Record<string, string> = {};
  for (const variable of row.variables ?? []) {
    if (variable.sample) {
      defaults[variable.name] = variable.sample;
    }
  }
  resetVars(detectedVars.value, defaults);
  formVisible.value = true;
}

async function handlePreview(): Promise<void> {
  if (!editingId.value) {
    return;
  }
  previewing.value = true;
  try {
    const vars: Record<string, string> = {};
    for (const name of detectedVars.value) {
      vars[name] = sampleVars[name] ?? '';
    }
    preview.value = await previewNoticeTemplate(editingId.value, vars);
  } catch (error) {
    showError(error);
  } finally {
    previewing.value = false;
  }
}

async function submitForm(): Promise<void> {
  submitting.value = true;
  try {
    const payload = {
      template_code: form.template_code.trim(),
      template_name: form.template_name.trim(),
      notice_type: form.notice_type,
      default_level: form.default_level,
      title_template: form.title_template,
      content_template: form.content_template,
      content_format: form.content_format,
      link_template: form.link_template,
      channels: form.channels,
      variables: detectedVars.value.map((name) => ({
        name,
        label: name,
        required: true,
        sample: sampleVars[name] || undefined,
      })),
      status: form.status,
    };
    if (isEdit.value) {
      await updateNoticeTemplate(editingId.value, payload);
      void MessagePlugin.success('已保存');
    } else {
      await createNoticeTemplate(payload);
      void MessagePlugin.success('模板已新增');
    }
    formVisible.value = false;
    await loadList();
  } catch (error) {
    showError(error);
  } finally {
    submitting.value = false;
  }
}

function confirmRemove(row: NoticeTemplateRes): void {
  const dialog = DialogPlugin.confirm({
    header: '删除模板',
    body: `确定删除模板「${row.template_name}」？若已被发送批次引用将无法删除。`,
    theme: 'danger',
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: () => {
      dialog.destroy();
      void (async () => {
        try {
          await deleteNoticeTemplate(row.id);
          void MessagePlugin.success('已删除');
          await loadList();
        } catch (error) {
          showError(error);
        }
      })();
    },
  });
}

// ==================== 发送 ====================
function goSend(): void {
  void router.push('/business/notice/send');
}

function goSendWith(row: NoticeTemplateRes): void {
  void router.push({ path: '/business/notice/send', query: { template: row.template_code } });
}

function showError(error: unknown): void {
  void MessagePlugin.error(error instanceof ApiError ? error.message : '请求失败，请稍后重试');
}

onMounted(() => {
  void loadList();
});
</script>

<style lang="less" scoped>
.notice-template-page {
  padding: 16px;

  .filter-card {
    margin-bottom: 12px;
  }

  .toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 12px;

    &__count {
      font-size: 13px;
      color: var(--td-text-color-secondary);
    }
  }

  .cell-name {
    font-weight: 600;
  }

  .cell-sub {
    margin-left: 8px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  .cell-empty {
    color: var(--td-text-color-placeholder);
  }

  .template-editor {
    display: flex;
    gap: 20px;

    &__main {
      flex: 1;
      min-width: 0;
    }

    &__side {
      width: 280px;
      min-width: 280px;
    }
  }

  .side-block {
    margin-bottom: 16px;
    padding: 12px;
    border: 1px solid var(--td-component-stroke);
    border-radius: 6px;

    &__title {
      margin-bottom: 8px;
      font-weight: 600;
      font-size: 13px;
    }
  }

  .var-row {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 8px;
  }

  .preview-card {
    margin-top: 8px;
    padding: 10px 12px;
    border-radius: 4px;
    background: var(--td-bg-color-page);

    &__title {
      margin-bottom: 6px;
      font-weight: 600;
      color: var(--td-text-color-primary);
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

    code {
      padding: 0 4px;
      border-radius: 2px;
      background: var(--td-bg-color-secondarycontainer);
    }
  }

  .editor-footer {
    display: flex;
    align-items: center;
    margin-top: 12px;
    padding-top: 16px;
    border-top: 1px solid var(--td-component-stroke);

    &__spacer {
      flex: 1;
    }
  }

  .empty-state {
    padding: 48px 0;
    text-align: center;

    &__icon {
      font-size: 48px;
      color: var(--td-text-color-placeholder);
    }

    &__title {
      margin: 12px 0 4px;
      font-size: 15px;
      color: var(--td-text-color-primary);
    }

    &__desc {
      margin-bottom: 16px;
      font-size: 13px;
      color: var(--td-text-color-secondary);
    }
  }
}
</style>

<template>
  <div class="rule-page" data-testid="attachment-rule-page">
    <t-card :bordered="false" class="filter-card">
      <t-row :gutter="16">
        <t-col :span="3">
          <t-select
            v-model="query.docType"
            placeholder="单据类型"
            clearable
            filterable
            data-testid="rule-filter-doc-type"
          >
            <t-option
              v-for="option in options.doc_types"
              :key="option.value"
              :value="option.value"
              :label="option.label"
            />
          </t-select>
        </t-col>
        <t-col :span="3">
          <t-select
            v-model="query.requiredAt"
            placeholder="触发动作"
            clearable
            data-testid="rule-filter-required-at"
          >
            <t-option
              v-for="option in options.required_ats"
              :key="option.value"
              :value="option.value"
              :label="option.label"
            />
          </t-select>
        </t-col>
        <t-col :span="3">
          <t-select v-model="query.enabled" placeholder="启用状态" clearable data-testid="rule-filter-enabled">
            <t-option :value="'true'" label="启用" />
            <t-option :value="'false'" label="禁用" />
          </t-select>
        </t-col>
        <t-col :span="3">
          <t-input
            v-model="query.keyword"
            placeholder="单据 / 角色 / 说明（模糊）"
            clearable
          />
        </t-col>
        <t-col :span="6">
          <t-space>
            <t-button theme="default" @click="handleReset">重置筛选</t-button>
            <t-button theme="default" @click="loadAll">
              <template #icon><t-icon name="refresh" /></template>
              刷新
            </t-button>
          </t-space>
        </t-col>
      </t-row>
    </t-card>

    <t-card :bordered="false" class="table-card">
      <div class="toolbar">
        <t-space>
          <t-button
            v-privilege="['support:attachment-rule:create']"
            theme="primary"
            data-testid="rule-create"
            @click="openCreate"
          >
            <template #icon><t-icon name="add" /></template>
            新增规则
          </t-button>
        </t-space>
        <span class="rule-page__count" data-testid="rule-count">
          共 {{ filteredRows.length }} 条（启用 {{ enabledCount }} 条 / 禁用 {{ filteredRows.length - enabledCount }} 条）
        </span>
      </div>

      <t-alert theme="info" :bordered="false" class="rule-page__hint">
        <template #message>
          规则改完<span class="rule-page__strong">立即生效</span>：单据的下一次提交 / 确认 / 生效就按新规则校验，不用重启、不发版。
          禁用某条规则 = 该类单据不再校验它；只有超级管理员能改（决定「哪些单据提交不了」属于内控配置）。
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="filteredRows"
        :columns="columns"
        :loading="loading"
        :hover="true"
        :stripe="true"
        data-testid="rule-table"
      >
        <template #doc_type="{ row }">
          <t-tag variant="light" theme="primary">{{ row.doc_type_label }}</t-tag>
          <span class="rule-page__code">{{ row.doc_type }}</span>
        </template>
        <template #file_role="{ row }">
          <span v-if="row.file_role_label">{{ row.file_role_label }}</span>
          <span v-else class="rule-page__muted">任意角色</span>
        </template>
        <template #required_at="{ row }">
          <t-tag variant="outline" :theme="row.required_at === 'ACTIVATE' ? 'warning' : 'default'">
            {{ row.required_at_label }}
          </t-tag>
        </template>
        <template #count="{ row }">
          <span class="rule-page__mono">{{ row.min_count }}</span>
          <span class="rule-page__muted"> ~ {{ row.max_count ?? '不限' }}</span>
        </template>
        <template #allow_exts="{ row }">
          <t-space v-if="row.allow_exts_list.length" size="4px" break-line>
            <t-tag v-for="ext in row.allow_exts_list" :key="ext" size="small" variant="light">
              {{ ext }}
            </t-tag>
          </t-space>
          <span v-else class="rule-page__muted">不限格式</span>
        </template>
        <template #remark="{ row }">
          <span v-if="row.remark">{{ row.remark }}</span>
          <span v-else class="rule-page__muted">—</span>
          <div class="rule-page__req" data-testid="rule-requirement">
            {{ requirementSummary(row.requirement, row.enabled) }}
          </div>
        </template>
        <template #enabled="{ row }">
          <t-switch
            :value="row.enabled"
            :loading="togglingId === row.id"
            :disabled="!canUpdate"
            :data-testid="`rule-toggle-${row.id}`"
            @change="(value: unknown) => toggleEnabled(row, Boolean(value))"
          />
        </template>
        <template #updated_at="{ row }">
          <span class="rule-page__muted">{{ formatDateTime(row.updated_at) }}</span>
        </template>
        <template #op="{ row }">
          <t-space size="small">
            <t-link
              v-privilege="['support:attachment-rule:update']"
              theme="primary"
              data-testid="rule-edit"
              @click="openEdit(row)"
            >
              编辑
            </t-link>
            <t-link
              v-privilege="['support:attachment-rule:delete']"
              theme="danger"
              data-testid="rule-delete"
              @click="confirmRemove(row)"
            >
              删除
            </t-link>
          </t-space>
        </template>
      </t-table>
    </t-card>

    <t-dialog
      v-model:visible="formVisible"
      :header="isEdit ? '编辑附件规则' : '新增附件规则'"
      :confirm-btn="{ content: '保存', theme: 'primary', loading: submitting }"
      width="640px"
      destroy-on-close
      :close-on-overlay-click="!submitting"
      data-testid="rule-form-dialog"
      @confirm="submitForm"
    >
      <t-form ref="formRef" :data="form" :rules="formRules" label-width="110px">
        <t-form-item label="单据类型" name="doc_type">
          <t-select v-model="form.doc_type" placeholder="请选择单据类型" filterable data-testid="rule-form-doc-type">
            <t-option
              v-for="option in options.doc_types"
              :key="option.value"
              :value="option.value"
              :label="`${option.label}（${option.value}）`"
            />
          </t-select>
        </t-form-item>
        <t-form-item label="触发动作" name="required_at">
          <t-radio-group v-model="form.required_at" data-testid="rule-form-required-at">
            <t-radio-button v-for="option in options.required_ats" :key="option.value" :value="option.value">
              {{ option.label }}
            </t-radio-button>
          </t-radio-group>
          <p class="form-tip">提交 = 单据提交 / 开票；确认 = 盘点确认；生效 = 资产启用、付款出款</p>
        </t-form-item>
        <t-form-item label="文件角色" name="file_role">
          <t-select v-model="form.file_role" placeholder="留空 = 任意角色都算" clearable data-testid="rule-form-file-role">
            <t-option
              v-for="option in options.file_roles"
              :key="option.value"
              :value="option.value"
              :label="`${option.label}（${option.value}）`"
            />
          </t-select>
          <p class="form-tip">留空 = 任意角色都算（只要数量与格式满足即可）</p>
        </t-form-item>
        <t-form-item label="数量区间" name="min_count">
          <t-space>
            <t-input-number
              v-model="form.min_count"
              theme="normal"
              :min="1"
              :max="99"
              data-testid="rule-form-min-count"
            />
            <span class="rule-page__muted">至少</span>
            <t-input-number
              v-model="form.max_count"
              theme="normal"
              :min="1"
              :max="99"
              placeholder="留空 = 不限"
              data-testid="rule-form-max-count"
            />
            <span class="rule-page__muted">个（右侧留空 = 不限）</span>
          </t-space>
        </t-form-item>
        <t-form-item label="允许格式" name="allow_exts">
          <t-input
            v-model="form.allow_exts"
            placeholder="逗号分隔，如 pdf,ofd,xml（留空 = 不限格式）"
            data-testid="rule-form-allow-exts"
            @blur="checkAllowExts"
          />
          <p class="form-tip">小写扩展名、逗号分隔、最多 20 个；留空 = 不限格式。</p>
          <p v-if="allowExtsError" class="form-error" data-testid="rule-form-allow-exts-error">
            {{ allowExtsError }}
          </p>
        </t-form-item>
        <t-form-item label="附加条件" name="condition_sql">
          <t-input
            v-model="form.condition_sql"
            placeholder="留空 = 恒适用，如 invoice_code IS NULL"
            data-testid="rule-form-condition"
          />
          <p class="form-tip">
            对单据表当前行求值的 SQL 片段（不含分号 / 注释），只在特殊分支用；
            普通规则留空。
          </p>
        </t-form-item>
        <t-form-item label="说明" name="remark">
          <t-input
            v-model="form.remark"
            placeholder="直接进错误文案，如「发票影像」"
            :maxlength="200"
            data-testid="rule-form-remark"
          />
          <p class="form-tip">提交被拦时会说「{{ previewRequirement }}」，改这里就是改提示语。</p>
        </t-form-item>
        <t-form-item label="启用" name="enabled">
          <t-switch v-model="form.enabled" data-testid="rule-form-enabled" />
          <span class="rule-page__muted rule-page__inline">禁用后该规则不参与校验（即时生效）</span>
        </t-form-item>
      </t-form>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import {
  DialogPlugin,
  MessagePlugin,
  type FormInstanceFunctions,
  type FormRules,
  type PrimaryTableCol,
} from 'tdesign-vue-next';
import {
  createAttachmentRule,
  deleteAttachmentRule,
  getAttachmentRuleOptions,
  listAttachmentRules,
  setAttachmentRuleEnabled,
  updateAttachmentRule,
  type AttachmentRuleRes,
  type AttachmentRuleSaveParams,
  type RuleOptionsRes,
} from '@/api/support/attachment-rule-api';
import { ApiError } from '@/api/request';
import { useUserStore } from '@/stores/user';
import { formatDateTime } from '@/utils/format';
import {
  parseAllowExts,
  requirementSummary,
  validateCounts,
} from './rule-pure';

/** 查询条件（前端过滤：规则条数是个位数，不值得为它加一套分页） */
interface RuleQuery {
  /** 单据类型（`DocKind` 字面值） */
  docType: string;
  /** 触发动作 */
  requiredAt: string;
  /** 启用状态（`'true'` / `'false'`；空 = 全部） */
  enabled: '' | 'true' | 'false';
  /** 关键字（单据 / 角色 / 说明模糊） */
  keyword: string;
}

/** 新增 / 编辑表单（`null` 语义与后端 PUT 全量替换一致） */
interface RuleForm {
  /** 规则 id，新增时为空 */
  id: string;
  /** 单据类型 */
  doc_type: string;
  /** 文件角色（空串 = 任意；提交时归一为 `null`） */
  file_role: string;
  /** 触发动作 */
  required_at: string;
  /** 至少几个 */
  min_count: number;
  /** 最多几个（`undefined` = 不限；提交时归一为 `null`） */
  max_count: number | undefined;
  /** 扩展名白名单原文 */
  allow_exts: string;
  /** 附加条件 */
  condition_sql: string;
  /** 说明 */
  remark: string;
  /** 是否启用 */
  enabled: boolean;
}

const userStore = useUserStore();
const canUpdate = computed(() => userStore.hasPermission('support:attachment-rule:update'));

const rows = ref<AttachmentRuleRes[]>([]);
const loading = ref(false);
const options = reactive<RuleOptionsRes>({ doc_types: [], required_ats: [], file_roles: [] });
const query = reactive<RuleQuery>({
  docType: '',
  requiredAt: '',
  enabled: '',
  keyword: '',
});

const columns: PrimaryTableCol[] = [
  { colKey: 'doc_type', title: '单据类型', width: 150, cell: 'doc_type' },
  { colKey: 'file_role', title: '文件角色', width: 120, cell: 'file_role' },
  { colKey: 'required_at', title: '触发动作', width: 100, cell: 'required_at' },
  { colKey: 'count', title: '数量', width: 110, cell: 'count' },
  { colKey: 'allow_exts', title: '允许格式', width: 200, cell: 'allow_exts' },
  { colKey: 'remark', title: '说明 / 提交时的提示语', ellipsis: false, cell: 'remark' },
  { colKey: 'enabled', title: '启用', width: 90, cell: 'enabled' },
  { colKey: 'updated_at', title: '更新时间', width: 170, cell: 'updated_at' },
  { colKey: 'op', title: '操作', width: 140, fixed: 'right' },
];

const filteredRows = computed(() => {
  const keyword = query.keyword.trim().toLowerCase();
  return rows.value.filter((row) => {
    if (query.docType && row.doc_type !== query.docType) {
      return false;
    }
    if (query.requiredAt && row.required_at !== query.requiredAt) {
      return false;
    }
    if (query.enabled && String(row.enabled) !== query.enabled) {
      return false;
    }
    if (!keyword) {
      return true;
    }
    return [row.doc_type, row.doc_type_label, row.file_role ?? '', row.file_role_label ?? '', row.remark ?? '']
      .join(' ')
      .toLowerCase()
      .includes(keyword);
  });
});

const enabledCount = computed(() => filteredRows.value.filter((row) => row.enabled).length);

const formVisible = ref(false);
const isEdit = ref(false);
const submitting = ref(false);
const togglingId = ref('');
const allowExtsError = ref('');
const formRef = ref<FormInstanceFunctions | null>(null);
const form = reactive<RuleForm>({
  id: '',
  doc_type: '',
  file_role: '',
  required_at: 'SUBMIT',
  min_count: 1,
  max_count: undefined,
  allow_exts: '',
  condition_sql: '',
  remark: '',
  enabled: true,
});
const formRules: FormRules<RuleForm> = {
  doc_type: [{ required: true, message: '请选择单据类型' }],
  required_at: [{ required: true, message: '请选择触发动作' }],
  min_count: [{ required: true, message: '请填写最少数量' }],
};

/** 表单里实时预览的「提交被拦时」文案（与后端 `requirement` 同构） */
const previewRequirement = computed(() => {
  const docLabel =
    options.doc_types.find((option) => option.value === form.doc_type)?.label ?? form.doc_type ?? '该单据';
  const roleLabel =
    options.file_roles.find((option) => option.value === form.file_role)?.label ?? '附件';
  const name = form.remark.trim() || roleLabel;
  const exts = parseAllowExts(form.allow_exts).value;
  return `${docLabel}必须上传${name}${exts ? `（${exts.split(',').join('/')}）` : ''}`;
});

function showError(message: string): void {
  void MessagePlugin.error(message);
}

async function loadAll(): Promise<void> {
  loading.value = true;
  try {
    const [list, dropdown] = await Promise.all([
      listAttachmentRules(),
      getAttachmentRuleOptions(),
    ]);
    rows.value = list.list;
    options.doc_types = dropdown.doc_types;
    options.required_ats = dropdown.required_ats;
    options.file_roles = dropdown.file_roles;
  } catch (error) {
    showError(error instanceof ApiError ? error.message : '附件规则加载失败');
  } finally {
    loading.value = false;
  }
}

function handleReset(): void {
  query.docType = '';
  query.requiredAt = '';
  query.enabled = '';
  query.keyword = '';
}

function resetForm(): void {
  form.id = '';
  form.doc_type = '';
  form.file_role = '';
  form.required_at = 'SUBMIT';
  form.min_count = 1;
  form.max_count = undefined;
  form.allow_exts = '';
  form.condition_sql = '';
  form.remark = '';
  form.enabled = true;
  allowExtsError.value = '';
}

function openCreate(): void {
  resetForm();
  isEdit.value = false;
  formVisible.value = true;
}

function openEdit(row: AttachmentRuleRes): void {
  resetForm();
  isEdit.value = true;
  form.id = row.id;
  form.doc_type = row.doc_type;
  form.file_role = row.file_role ?? '';
  form.required_at = row.required_at;
  form.min_count = row.min_count;
  form.max_count = row.max_count ?? undefined;
  form.allow_exts = row.allow_exts ?? '';
  form.condition_sql = row.condition_sql ?? '';
  form.remark = row.remark ?? '';
  form.enabled = row.enabled;
  formVisible.value = true;
}

function checkAllowExts(): void {
  allowExtsError.value = parseAllowExts(form.allow_exts).error ?? '';
}

/** 表单 → 后端入参（空串一律归一成 `null`，与 PUT 全量替换语义一致） */
function toPayload(): AttachmentRuleSaveParams {
  return {
    doc_type: form.doc_type,
    file_role: form.file_role || null,
    required_at: form.required_at,
    min_count: form.min_count,
    max_count: form.max_count ?? null,
    allow_exts: parseAllowExts(form.allow_exts).value,
    condition_sql: form.condition_sql.trim() || null,
    remark: form.remark.trim() || null,
    enabled: form.enabled,
  };
}

async function submitForm(): Promise<void> {
  if (submitting.value) {
    return;
  }
  const invalid = await formRef.value?.validate();
  if (invalid !== true) {
    return;
  }
  const exts = parseAllowExts(form.allow_exts);
  if (exts.error) {
    allowExtsError.value = exts.error;
    return;
  }
  const counts = validateCounts(form.min_count, form.max_count ?? null);
  if (counts.error) {
    showError(counts.error);
    return;
  }

  submitting.value = true;
  try {
    const payload = toPayload();
    if (isEdit.value) {
      await updateAttachmentRule(form.id, payload);
      void MessagePlugin.success('规则已更新，下次提交立即按新规则校验');
    } else {
      await createAttachmentRule(payload);
      void MessagePlugin.success('规则已新增，下次提交立即按新规则校验');
    }
    formVisible.value = false;
    await loadAll();
  } catch (error) {
    showError(error instanceof ApiError ? error.message : '保存失败');
  } finally {
    submitting.value = false;
  }
}

async function toggleEnabled(row: AttachmentRuleRes, next: boolean): Promise<void> {
  if (togglingId.value) {
    return;
  }
  togglingId.value = row.id;
  const previous = row.enabled;
  row.enabled = next;
  try {
    const updated = await setAttachmentRuleEnabled(row.id, { enabled: next });
    row.enabled = updated.enabled;
    void MessagePlugin.success(next ? '已启用：下一次提交开始校验' : '已禁用：该规则不再参与校验');
  } catch (error) {
    row.enabled = previous;
    showError(error instanceof ApiError ? error.message : '状态切换失败');
  } finally {
    togglingId.value = '';
  }
}

function confirmRemove(row: AttachmentRuleRes): void {
  const dialog = DialogPlugin.confirm({
    header: '删除附件规则',
    body: `确定删除「${row.doc_type_label} · ${row.required_at_label} · ${row.remark || '附件'}」这条规则？删除后该单据不再被它校验（软删，可重新新建）。`,
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: async () => {
      dialog.hide();
      try {
        await deleteAttachmentRule(row.id);
        void MessagePlugin.success('已删除');
        await loadAll();
      } catch (error) {
        showError(error instanceof ApiError ? error.message : '删除失败');
      }
    },
  });
}

onMounted(() => {
  void loadAll();
});
</script>

<style scoped>
.rule-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.filter-card,
.table-card {
  padding: 4px;
}
.toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 12px;
}
.rule-page__count {
  color: var(--td-text-color-secondary);
  font-size: 13px;
}
.rule-page__hint {
  margin-bottom: 12px;
}
.rule-page__strong {
  font-weight: 600;
  color: var(--td-brand-color);
}
.rule-page__code {
  margin-left: 6px;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}
.rule-page__muted {
  color: var(--td-text-color-placeholder);
}
.rule-page__inline {
  margin-left: 8px;
}
.rule-page__mono {
  font-family: var(--td-font-family-mono, monospace);
}
.rule-page__req {
  margin-top: 2px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}
.form-tip {
  margin: 4px 0 0;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
  line-height: 18px;
}
.form-error {
  margin: 4px 0 0;
  color: var(--td-error-color);
  font-size: 12px;
}
</style>

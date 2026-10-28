<template>
  <div class="masking-page">
    <!-- ========== 筛选 ========== -->
    <t-card :bordered="false" class="filter-card">
      <t-row :gutter="16">
        <t-col :span="4">
          <t-select
            v-model="query.table_name"
            placeholder="表名（精确）"
            clearable
            filterable
            creatable
            :options="tableOptions"
            @change="handleSearch"
          />
        </t-col>
        <t-col :span="3">
          <t-input v-model="query.column_name" placeholder="字段名（精确）" clearable @enter="handleSearch" />
        </t-col>
        <t-col :span="3">
          <t-select v-model="query.scope" placeholder="作用域" clearable>
            <t-option value="global" label="全局规则" />
            <t-option value="role" label="角色规则" />
          </t-select>
        </t-col>
        <t-col :span="2">
          <t-select v-model="query.status" placeholder="状态" clearable>
            <t-option :value="1" label="启用" />
            <t-option :value="0" label="停用" />
          </t-select>
        </t-col>
        <t-col :span="4">
          <t-space>
            <t-button theme="primary" @click="handleSearch">
              <template #icon><t-icon name="search" /></template>
              查询
            </t-button>
            <t-button theme="default" @click="handleReset">重置</t-button>
            <t-button theme="default" :loading="loading" @click="loadList">
              <template #icon><t-icon name="refresh" /></template>
              刷新
            </t-button>
          </t-space>
        </t-col>
      </t-row>
    </t-card>

    <!-- ========== 列表 ========== -->
    <t-card :bordered="false" class="table-card">
      <div class="toolbar">
        <t-space>
          <t-button v-privilege="[PERM.fieldMaskCreate]" theme="primary" @click="openCreate">
            <template #icon><t-icon name="add" /></template>
            新增规则
          </t-button>
        </t-space>
        <span class="toolbar__count">共 {{ pagination.total }} 条规则</span>
      </div>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        stripe
        @page-change="handlePageChange"
      >
        <template #table_name="{ row }">
          <span class="cell-mono">{{ row.table_name }}</span>
        </template>
        <template #column_name="{ row }">
          <span class="cell-mono cell-strong">{{ row.column_name }}</span>
        </template>
        <template #mask_type="{ row }">
          <t-tag theme="primary" variant="light" size="small">{{ row.mask_type_label }}</t-tag>
          <span v-if="row.mask_param" class="cell-sub">{{ row.mask_param }}</span>
        </template>
        <template #role="{ row }">
          <t-tag v-if="row.scope === 'GLOBAL'" theme="warning" variant="light" size="small">全局</t-tag>
          <t-tag v-else theme="success" variant="light" size="small">{{ row.role_name || '角色已删除' }}</t-tag>
        </template>
        <template #status="{ row }">
          <t-tag :theme="row.status === 1 ? 'success' : 'default'" variant="light" size="small">
            {{ row.status === 1 ? '启用' : '停用' }}
          </t-tag>
        </template>
        <template #updated_at="{ row }">{{ formatDateTime(row.updated_at) }}</template>
        <template #op="{ row }">
          <t-space size="small">
            <t-link v-privilege="[PERM.fieldMaskUpdate]" theme="primary" @click="openEdit(row)">编辑</t-link>
            <t-link
              v-privilege="[PERM.fieldMaskUpdate]"
              :theme="row.status === 1 ? 'warning' : 'success'"
              @click="onToggleStatus(row, row.status !== 1)"
            >
              {{ row.status === 1 ? '停用' : '启用' }}
            </t-link>
            <t-link v-privilege="[PERM.fieldMaskDelete]" theme="danger" @click="confirmRemove(row)">删除</t-link>
          </t-space>
        </template>
        <template #empty>
          <div class="empty-state">
            <t-icon class="empty-state__icon" name="mask" />
            <p class="empty-state__title">还没有脱敏规则</p>
            <p class="empty-state__desc">一条规则 = 一张表的某个字段 + 一种脱敏类型 + 生效角色</p>
            <t-button v-privilege="[PERM.fieldMaskCreate]" theme="primary" @click="openCreate">
              新增规则
            </t-button>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ========== 新增 / 编辑 ========== -->
    <t-drawer
      v-model:visible="formVisible"
      :header="isEdit ? `编辑脱敏规则 · ${form.table_name}.${form.column_name}` : '新增脱敏规则'"
      size="760px"
      :footer="false"
      destroy-on-close
    >
      <div class="mask-editor">
        <div class="mask-editor__main">
          <t-form :data="form" label-width="96px">
            <t-form-item label="表名" required>
              <t-select
                v-model="form.table_name"
                :disabled="isEdit"
                :options="tableOptions"
                filterable
                creatable
                placeholder="选表或直接输入表名（如 sys_user）"
                @change="onTableChange"
              />
              <div class="field-hint">
                {{ isEdit ? '表名 / 字段 / 适用角色不可改：换目标请删除后重建（否则审计追溯失效）' : '合法性的唯一权威是后端：表或列不存在会返回 61006' }}
              </div>
            </t-form-item>

            <t-form-item label="字段" required>
              <t-select
                v-model="form.column_name"
                :disabled="isEdit"
                :options="columnOptions"
                filterable
                creatable
                placeholder="选字段或直接输入列名（如 mobile）"
              />
            </t-form-item>

            <t-form-item label="脱敏类型" required>
              <t-select v-model="form.mask_type" :options="typeOptions" @change="onTypeChange" />
              <div class="field-hint">{{ currentTypeHint }}</div>
            </t-form-item>

            <t-form-item label="类型参数">
              <t-input
                v-model="form.mask_param"
                :disabled="form.mask_type === 3"
                :placeholder="form.mask_type === 3 ? '邮箱类型无参数' : currentTypeParamHint"
              />
              <div class="field-hint" :class="{ 'field-hint--error': !!paramError }">
                <template v-if="paramError">{{ paramError }}</template>
                <template v-else>留空 = 使用该类型缺省参数<template v-if="defaultParamText">（{{ defaultParamText }}）</template></template>
              </div>
            </t-form-item>

            <t-form-item label="适用角色">
              <t-select v-model="form.role_id" :disabled="isEdit" placeholder="不选 = 全局规则（对所有未豁免人生效）" clearable>
                <t-option value="" label="全局（所有未豁免用户）" />
                <t-option v-for="role in roleOptions" :key="role.id" :value="role.id" :label="role.role_name" />
              </t-select>
            </t-form-item>

            <t-form-item label="脱敏理由" required>
              <t-textarea
                v-model="form.reason"
                :autosize="{ minRows: 2 }"
                placeholder="合规留痕：写清为什么要脱这一列（审计要能回答这个问题）"
                :maxlength="200"
              />
            </t-form-item>

            <t-form-item label="状态">
              <t-radio-group v-model="form.status">
                <t-radio :value="1">启用</t-radio>
                <t-radio :value="0">停用</t-radio>
              </t-radio-group>
            </t-form-item>
          </t-form>

          <div class="mask-editor__footer">
            <t-space>
              <t-button theme="primary" :loading="submitting" @click="handleSubmit">
                {{ isEdit ? '保存' : '创建' }}
              </t-button>
              <t-button theme="default" @click="formVisible = false">取消</t-button>
            </t-space>
          </div>
        </div>

        <!-- 右侧：脱敏预览（调后端试运行接口，结果与运行时完全一致） -->
        <div class="mask-editor__side">
          <div class="side-block">
            <div class="side-block__title">脱敏预览</div>
            <t-input
              v-model="previewInput"
              placeholder="输入一段样例值，实时看脱敏结果"
              clearable
            />
            <div class="preview-actions">
              <t-link theme="primary" size="small" @click="previewInput = currentSample">用该类型样例值</t-link>
            </div>
            <t-loading :loading="previewLoading" size="small">
              <div class="preview-card">
                <div class="preview-card__row">
                  <span class="preview-card__label">原文</span>
                  <span class="preview-card__value">{{ previewInput || '—' }}</span>
                </div>
                <div class="preview-card__row">
                  <span class="preview-card__label">脱敏后</span>
                  <span class="preview-card__value preview-card__value--out">{{ previewOutput || '—' }}</span>
                </div>
              </div>
            </t-loading>
            <div v-if="previewError" class="field-hint field-hint--error">{{ previewError }}</div>
            <div v-else-if="previewResult" class="field-hint">
              <t-tag v-if="previewResult.changed" theme="success" variant="light" size="small">已脱敏</t-tag>
              <t-tag v-else theme="warning" variant="light" size="small">无需脱敏</t-tag>
              <span class="preview-card__meta">{{ previewResult.mask_type_label }} · 参数 {{ previewResult.mask_param || '缺省' }}</span>
            </div>
          </div>

          <div class="side-block">
            <div class="side-block__title">规则提示</div>
            <ul class="tips">
              <li>同表同字段同角色只能有一条启用规则（重复 → 409）</li>
              <li>角色规则优先于全局规则；多角色命中时取更严格的那条</li>
              <li>停用一条规则 = 该批人立刻看到明文，属敏感操作</li>
              <li>超管与持 <code>system:field-mask:plain</code> 的人不受脱敏约束</li>
            </ul>
          </div>
        </div>
      </div>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';
import type { PrimaryTableCol, SelectOption } from 'tdesign-vue-next';
import {
  FIELD_SUGGESTIONS,
  MASK_TYPE_OPTIONS,
  TABLE_SUGGESTIONS,
  createFieldMask,
  deleteFieldMask,
  pageFieldMasks,
  previewMask,
  updateFieldMask,
  type FieldMaskRes,
  type MaskPreviewRes,
} from '@/api/system/field-mask-api';
import { listRoles, type RoleRes } from '@/api/system/role-api';
import { ApiError } from '@/api/request';
import { PERM } from '@/constants/permissions';
import { formatDateTime } from '@/utils/format';

// ===== 列表 =====
const loading = ref(false);
const rows = ref<FieldMaskRes[]>([]);
const pagination = reactive({ current: 1, pageSize: 10, total: 0 });
const query = reactive<{
  table_name: string;
  column_name: string;
  scope: 'global' | 'role' | '';
  status: number | undefined;
}>({ table_name: '', column_name: '', scope: '', status: undefined });

const statusLoading = ref('');

const columns: PrimaryTableCol[] = [
  { colKey: 'table_name', title: '表名', width: 170 },
  { colKey: 'column_name', title: '字段', width: 150 },
  { colKey: 'mask_type', title: '脱敏类型', width: 190 },
  { colKey: 'role', title: '适用角色', width: 160 },
  { colKey: 'status', title: '状态', width: 90 },
  { colKey: 'updated_at', title: '更新时间', width: 170 },
  { colKey: 'op', title: '操作', width: 180, fixed: 'right' },
];

/** 表名下拉项（建议清单；允许自由输入） */
const tableOptions: SelectOption[] = TABLE_SUGGESTIONS.map((name) => ({ label: name, value: name }));
/** 脱敏类型下拉项 */
const typeOptions: SelectOption[] = MASK_TYPE_OPTIONS.map((item) => ({
  label: `${item.label}（${item.code}）`,
  value: item.value,
}));

const roleOptions = ref<RoleRes[]>([]);

/** 表格分页回调参数（TDesign 未导出该类型，与项目其它页面同一口径） */
interface TablePageInfo {
  /** 当前页 */
  current: number;
  /** 每页条数 */
  pageSize: number;
}

/** 统一错误提示：保留后端原文（61005/61006/61007 的文案必须露出来） */
function showError(error: unknown, fallback: string): void {
  void MessagePlugin.error(error instanceof ApiError ? error.message || fallback : fallback);
}

async function loadList(): Promise<void> {
  loading.value = true;
  try {
    const data = await pageFieldMasks({
      page: pagination.current,
      page_size: pagination.pageSize,
      table_name: query.table_name || undefined,
      column_name: query.column_name || undefined,
      scope: query.scope || undefined,
      status: query.status,
    });
    rows.value = data.list;
    pagination.total = data.total;
  } catch (error) {
    rows.value = [];
    pagination.total = 0;
    showError(error, '脱敏规则加载失败');
  } finally {
    loading.value = false;
  }
}

async function loadRoles(): Promise<void> {
  try {
    roleOptions.value = await listRoles();
  } catch {
    // 角色列表拿不到不影响列表页，只是表单里选不了角色规则
    roleOptions.value = [];
  }
}

function handleSearch(): void {
  pagination.current = 1;
  void loadList();
}

function handleReset(): void {
  query.table_name = '';
  query.column_name = '';
  query.scope = '';
  query.status = undefined;
  handleSearch();
}

function handlePageChange(pageInfo: TablePageInfo): void {
  pagination.current = pageInfo.current;
  pagination.pageSize = pageInfo.pageSize;
  void loadList();
}

// ===== 表单 =====
const formVisible = ref(false);
const isEdit = ref(false);
const editingId = ref('');
const submitting = ref(false);
interface MaskForm {
  /** 表名 */
  table_name: string;
  /** 列名 */
  column_name: string;
  /** 脱敏类型（1..7） */
  mask_type: number;
  /** 类型参数 */
  mask_param: string;
  /** 适用角色；'' = 全局 */
  role_id: string;
  /** 理由 */
  reason: string;
  /** 状态 */
  status: number;
}
const form = reactive<MaskForm>({
  table_name: '',
  column_name: '',
  mask_type: 1,
  mask_param: '',
  role_id: '',
  reason: '',
  status: 1,
});

/** 当前类型的元数据 */
const currentType = computed(() => MASK_TYPE_OPTIONS.find((item) => item.value === form.mask_type));
const currentTypeHint = computed(() => currentType.value?.paramHint ?? '');
const currentTypeParamHint = computed(() => currentType.value?.paramHint ?? '');
const currentSample = computed(() => currentType.value?.sample ?? '');
const defaultParamText = computed(() => currentType.value?.defaultParam ?? '');

/** 字段名下拉项：随选定表变化 */
const columnOptions = computed<SelectOption[]>(() => {
  const list = FIELD_SUGGESTIONS[form.table_name] ?? [];
  return list.map((name) => ({ label: name, value: name }));
});

/** 本地参数校验（与后端 `masker` 的解析口径逐条对齐；权威仍是后端） */
const paramError = computed<string>(() => {
  const type = form.mask_type;
  const text = form.mask_param.trim();
  if (type === 3) {
    return text ? '邮箱脱敏没有参数，请留空' : '';
  }
  if (!text) {
    return type === 7 ? '自定义正则必须填写：格式为「正则|保留组」，如 ^(\\w{2}).*$|1' : '';
  }
  if (type === 7) {
    // 后端用 rsplit_once('|')：最后一个 | 之后是保留组，之前是正则本体
    const index = text.lastIndexOf('|');
    const pattern = (index >= 0 ? text.slice(0, index) : text).trim();
    const group = (index >= 0 ? text.slice(index + 1) : '0').trim();
    if (!pattern) {
      return '正则表达式不能为空';
    }
    if (!/^\d+$/.test(group)) {
      return `保留组「${group}」不是非负整数`;
    }
    try {
      // 只做语法体检；Rust regex 与 JS RegExp 语法略有差异，最终以后端为准
      new RegExp(pattern);
    } catch (error) {
      return `正则表达式不合法：${(error as Error).message}`;
    }
    return '';
  }
  if (type === 1 || type === 2 || type === 5) {
    return /^\d+,\d+$/.test(text) ? '' : '参数格式应为「保留前N,后M」，如 3,4';
  }
  return /^\d+$/.test(text) ? '' : '参数应为非负整数，如 1（姓名）或 1000（金额）';
});

// ===== 预览 =====
const previewInput = ref('');
const previewOutput = ref('');
const previewResult = ref<MaskPreviewRes | null>(null);
const previewError = ref('');
const previewLoading = ref(false);
/** 防抖定时器与请求序号：避免旧响应覆盖新输入 */
let previewTimer: ReturnType<typeof setTimeout> | null = null;
let previewSeq = 0;

async function runPreview(): Promise<void> {
  const value = previewInput.value;
  if (!value.trim()) {
    previewOutput.value = '';
    previewResult.value = null;
    previewError.value = '';
    return;
  }
  if (paramError.value) {
    previewOutput.value = '';
    previewResult.value = null;
    previewError.value = `预览失败：${paramError.value}`;
    return;
  }
  const seq = ++previewSeq;
  previewLoading.value = true;
  try {
    const res = await previewMask({
      mask_type: form.mask_type,
      mask_param: form.mask_param.trim() || undefined,
      value,
    });
    if (seq !== previewSeq) {
      return;
    }
    previewResult.value = res;
    previewOutput.value = res.output;
    previewError.value = '';
  } catch (error) {
    if (seq !== previewSeq) {
      return;
    }
    previewResult.value = null;
    previewOutput.value = '';
    previewError.value = error instanceof ApiError ? error.message : '预览失败';
  } finally {
    if (seq === previewSeq) {
      previewLoading.value = false;
    }
  }
}

/** 输入 / 类型 / 参数变化 → 350ms 防抖后实时预览 */
watch(
  () => [previewInput.value, form.mask_type, form.mask_param],
  () => {
    if (previewTimer) {
      clearTimeout(previewTimer);
    }
    previewTimer = setTimeout(() => {
      void runPreview();
    }, 350);
  },
);

// ===== 打开表单 =====
function resetForm(): void {
  Object.assign(form, {
    table_name: '',
    column_name: '',
    mask_type: 1,
    mask_param: '',
    role_id: '',
    reason: '',
    status: 1,
  });
  previewInput.value = MASK_TYPE_OPTIONS[0].sample;
  previewOutput.value = '';
  previewResult.value = null;
  previewError.value = '';
}

function openCreate(): void {
  isEdit.value = false;
  editingId.value = '';
  resetForm();
  formVisible.value = true;
  void runPreview();
}

function openEdit(row: FieldMaskRes): void {
  isEdit.value = true;
  editingId.value = row.id;
  Object.assign(form, {
    table_name: row.table_name,
    column_name: row.column_name,
    mask_type: row.mask_type,
    mask_param: row.mask_param ?? '',
    role_id: row.role_id ?? '',
    reason: row.reason,
    status: row.status,
  });
  previewInput.value = MASK_TYPE_OPTIONS.find((item) => item.value === row.mask_type)?.sample ?? '';
  previewOutput.value = '';
  previewResult.value = null;
  previewError.value = '';
  formVisible.value = true;
  void runPreview();
}

/** 表名变化：清掉不属于新表建议清单的字段（避免提交一个明显的错组合） */
function onTableChange(): void {
  const list = FIELD_SUGGESTIONS[form.table_name] ?? [];
  if (list.length > 0 && !list.includes(form.column_name)) {
    form.column_name = '';
  }
}

/** 类型变化：换成该类型的缺省参数与样例值 */
function onTypeChange(): void {
  const type = MASK_TYPE_OPTIONS.find((item) => item.value === form.mask_type);
  form.mask_param = type?.defaultParam ?? '';
  if (type) {
    previewInput.value = type.sample;
  }
}

async function handleSubmit(): Promise<void> {
  if (!form.table_name.trim() || !form.column_name.trim()) {
    void MessagePlugin.warning('请先选择表名与字段');
    return;
  }
  if (!form.reason.trim()) {
    void MessagePlugin.warning('脱敏理由必填（合规留痕）');
    return;
  }
  if (paramError.value) {
    void MessagePlugin.warning(paramError.value);
    return;
  }
  submitting.value = true;
  try {
    if (isEdit.value) {
      await updateFieldMask(editingId.value, {
        mask_type: form.mask_type,
        mask_param: form.mask_param.trim() || undefined,
        status: form.status,
        reason: form.reason.trim(),
      });
      void MessagePlugin.success('规则已更新');
    } else {
      await createFieldMask({
        table_name: form.table_name.trim(),
        column_name: form.column_name.trim(),
        mask_type: form.mask_type,
        mask_param: form.mask_param.trim() || undefined,
        role_id: form.role_id || undefined,
        status: form.status,
        reason: form.reason.trim(),
      });
      void MessagePlugin.success('规则已创建');
    }
    formVisible.value = false;
    await loadList();
  } catch (error) {
    // 61005 参数非法 / 61006 表或列不存在 / 61007 正则非法 / 409 重复规则
    showError(error, '保存规则失败');
  } finally {
    submitting.value = false;
  }
}

// ===== 启停 =====
function onToggleStatus(row: FieldMaskRes, enabled: boolean): void {
  if (enabled) {
    void applyStatus(row, true);
    return;
  }
  // PRD §6.2：停用脱敏规则 = 全员立刻看到明文，必须二次确认并写明影响面
  const scopeText =
    row.scope === 'GLOBAL'
      ? '所有未豁免用户（含全部非超管角色）'
      : `持有角色「${row.role_name ?? '已删除角色'}」的用户`;
  const dialog = DialogPlugin.confirm({
    theme: 'warning',
    header: '确认停用该脱敏规则？',
    body: `停用后 ${scopeText} 将看到 ${row.table_name}.${row.column_name} 的明文。敏感操作不可撤销式提示，请确认确有必要。`,
    confirmBtn: '确认停用',
    cancelBtn: '取消',
    onConfirm: () => {
      dialog.destroy();
      void applyStatus(row, false);
    },
    onClose: () => dialog.destroy(),
  });
}

async function applyStatus(row: FieldMaskRes, enabled: boolean): Promise<void> {
  statusLoading.value = row.id;
  try {
    await updateFieldMask(row.id, {
      mask_type: row.mask_type,
      mask_param: row.mask_param || undefined,
      status: enabled ? 1 : 0,
      reason: row.reason,
    });
    row.status = enabled ? 1 : 0;
    void MessagePlugin.success(enabled ? '规则已启用' : '规则已停用');
    // 以库为准重新拉取（避免本地行状态与后端漂移）
    await loadList();
  } catch (error) {
    showError(error, '状态修改失败');
    // 失败时用后端数据回正（switch 是受控的，重载列表最稳）
    await loadList();
  } finally {
    statusLoading.value = '';
  }
}

// ===== 删除 =====
function confirmRemove(row: FieldMaskRes): void {
  const dialog = DialogPlugin.confirm({
    theme: 'warning',
    header: '确认删除该脱敏规则？',
    body: `将软删 ${row.table_name}.${row.column_name}（${row.mask_type_label}${row.scope === 'GLOBAL' ? '·全局' : `·${row.role_name ?? '角色已删除'}`}）。删除后该字段将不再脱敏，历史记录仍可在审计日志追溯。`,
    confirmBtn: '确认删除',
    cancelBtn: '取消',
    onConfirm: async () => {
      dialog.destroy();
      try {
        await deleteFieldMask(row.id);
        void MessagePlugin.success('规则已删除');
        await loadList();
      } catch (error) {
        showError(error, '删除失败');
      }
    },
    onClose: () => dialog.destroy(),
  });
}

onMounted(() => {
  void loadList();
  void loadRoles();
});
</script>

<style lang="less" scoped>
.masking-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.filter-card,
.table-card {
  :deep(.t-card__body) {
    padding: 16px;
  }
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

.cell-mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 12px;
}

.cell-strong {
  font-weight: 600;
}

.cell-sub {
  margin-left: 6px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.mask-editor {
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

  &__footer {
    display: flex;
    justify-content: flex-start;
    margin-top: 8px;
    padding-top: 16px;
    border-top: 1px solid var(--td-component-stroke);
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
    color: var(--td-text-color-primary);
  }
}

.preview-actions {
  margin: 6px 0 8px;
}

.preview-card {
  padding: 10px 12px;
  border-radius: 4px;
  background: var(--td-bg-color-page);

  &__row {
    display: flex;
    align-items: baseline;
    gap: 8px;
    margin-bottom: 6px;

    &:last-child {
      margin-bottom: 0;
    }
  }

  &__label {
    min-width: 48px;
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }

  &__value {
    word-break: break-all;
    font-size: 13px;
    color: var(--td-text-color-primary);
  }

  &__value--out {
    font-weight: 600;
    color: var(--td-brand-color);
  }

  &__meta {
    margin-left: 8px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
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

.tips {
  margin: 0;
  padding-left: 16px;
  font-size: 12px;
  line-height: 1.8;
  color: var(--td-text-color-secondary);

  code {
    padding: 0 4px;
    border-radius: 2px;
    background: var(--td-bg-color-secondarycontainer);
  }
}

.empty-state {
  padding: 40px 0;
  text-align: center;

  &__icon {
    font-size: 44px;
    color: var(--td-text-color-placeholder);
  }

  &__title {
    margin: 12px 0 4px;
    font-size: 15px;
    color: var(--td-text-color-primary);
  }

  &__desc {
    margin-bottom: 14px;
    font-size: 13px;
    color: var(--td-text-color-secondary);
  }
}
</style>

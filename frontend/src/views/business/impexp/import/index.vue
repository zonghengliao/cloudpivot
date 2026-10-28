<template>
  <div class="import-wizard-page">
    <t-card :bordered="false">
      <t-steps :current="step" class="wizard-steps">
        <t-step-item title="上传文件" content="选择业务与 Excel 文件" />
        <t-step-item title="校验报告" content="检查失败明细（行号 / 列 / 原因）" />
        <t-step-item title="确认入库" content="确认后写库并给结果" />
      </t-steps>

      <!-- ============ 第 1 步：上传 ============ -->
      <div v-if="step === 0" class="step-body">
        <t-form label-width="110px" class="step-form">
          <t-form-item label="导入业务">
            <t-select
              v-model="bizType"
              :options="bizOptions"
              :loading="defLoading"
              placeholder="请选择要导入的业务"
              class="step-form__select"
              @change="onBizChange"
            />
          </t-form-item>
          <t-form-item label="导入模式">
            <t-radio-group v-model="mode">
              <t-radio value="STRICT">严格模式（有失败行即整体中止）</t-radio>
              <t-radio value="PARTIAL" :disabled="!currentDef?.allow_partial">允许部分导入</t-radio>
            </t-radio-group>
          </t-form-item>
          <t-form-item label="模板">
            <t-space>
              <t-button
                v-privilege="[PERM.impexpImportTemplate]"
                variant="outline"
                :disabled="!bizType"
                @click="handleDownloadTemplate"
              >
                <template #icon><t-icon name="download" /></template>
                下载导入模板
              </t-button>
              <span class="hint">请按模板表头填写，单次最多 {{ (currentDef?.max_rows ?? 0).toLocaleString() }} 行</span>
            </t-space>
          </t-form-item>
        </t-form>

        <div
          class="drop-zone"
          :class="{ 'drop-zone--active': dragging }"
          @dragover.prevent="dragging = true"
          @dragleave.prevent="dragging = false"
          @drop.prevent="onDrop"
        >
          <t-icon class="drop-zone__icon" name="cloud-upload" />
          <p class="drop-zone__title">把 Excel 拖到这里，或</p>
          <input ref="fileInputRef" class="file-input" type="file" accept=".xlsx,.xls" @change="onFilePicked" />
          <t-button v-privilege="[PERM.impexpImportUpload]" theme="primary" :disabled="!bizType" @click="pickFile">
            选择文件
          </t-button>
          <p class="drop-zone__desc">支持 .xlsx / .xls，上传后立即做结构校验与逐行校验</p>
        </div>

        <div v-if="pickedFile" class="picked">
          <t-icon name="file-excel" />
          <span class="picked__name">{{ pickedFile.name }}</span>
          <span class="picked__size">{{ formatFileSize(pickedFile.size) }}</span>
          <t-button theme="primary" :loading="uploading" @click="startUpload">开始校验</t-button>
        </div>

        <t-alert v-if="stepError" class="step-alert" theme="error" :message="stepError" />
      </div>

      <!-- ============ 第 2 步：校验报告 ============ -->
      <div v-else-if="step === 1 && report" class="step-body">
        <div class="summary">
          <div class="summary__item">
            <span class="summary__label">数据总行数</span>
            <span class="summary__value">{{ report.total.toLocaleString() }}</span>
          </div>
          <div class="summary__item summary__item--ok">
            <span class="summary__label">校验通过</span>
            <span class="summary__value">{{ report.valid.toLocaleString() }}</span>
          </div>
          <div class="summary__item summary__item--error">
            <span class="summary__label">校验失败</span>
            <span class="summary__value">{{ report.error.toLocaleString() }}</span>
          </div>
          <div class="summary__item">
            <span class="summary__label">跳过示例行 / 空行</span>
            <span class="summary__value">{{ report.skipped_example }} / {{ report.skipped_blank }}</span>
          </div>
          <div class="summary__item">
            <span class="summary__label">校验耗时</span>
            <span class="summary__value">{{ report.elapsed_ms }} ms</span>
          </div>
        </div>

        <t-alert
          v-for="warning in report.warnings"
          :key="warning.code"
          class="step-alert"
          theme="warning"
          :message="`${warning.code}：${warning.message}`"
        />

        <t-alert
          v-if="report.error > 0 && mode === 'STRICT'"
          class="step-alert"
          theme="error"
          :message="`存在 ${report.error} 行校验失败，本批次未开启「允许部分导入」，已全部中止，请修正后重传（E-IMP-19）`"
        />

        <div class="fail-table">
          <div class="fail-table__head">
            <span>失败明细（行号 / 列 / 原因）</span>
            <t-space>
              <span class="hint">共 {{ report.error }} 行失败，表格最多展示 {{ report.errors.length }} 条</span>
              <t-button
                v-if="report.error > 0"
                v-privilege="[PERM.impexpImportDownload]"
                size="small"
                variant="outline"
                @click="handleDownloadErrors"
              >
                下载错误明细
              </t-button>
            </t-space>
          </div>
          <t-table
            row-key="row_no"
            :data="report.errors"
            :columns="errorColumns"
            :max-height="420"
            size="small"
            :stripe="true"
          >
            <template #row_no="{ row }">第 {{ row.row_no }} 行</template>
            <template #column="{ row }">{{ row.column_header || row.column_key || '—' }}</template>
            <template #raw_value="{ row }">
              <span class="cell-mono">{{ row.raw_value ?? '（空）' }}</span>
            </template>
            <template #error_message="{ row }">{{ row.error_message }}</template>
          </t-table>
          <p v-if="report.truncated" class="hint">
            明细已截断（服务端仅返回前 {{ report.errors.length }} 条），完整明细请下载错误文件
          </p>
        </div>

        <div class="step-actions">
          <t-button theme="default" @click="restart">重新上传</t-button>
          <t-button theme="primary" :disabled="!canConfirm" @click="goConfirm">下一步：确认入库</t-button>
        </div>
      </div>

      <!-- ============ 第 3 步：确认入库 ============ -->
      <div v-else class="step-body">
        <t-alert
          class="step-alert"
          theme="info"
          :message="`即将把 ${report?.valid ?? 0} 行数据写入业务表，任务号 ${taskNo}。入库过程中不可撤回。`"
        />
        <t-alert v-if="confirmResult" class="step-alert" :theme="confirmResult.status === 'SUCCESS' ? 'success' : 'warning'">
          {{ confirmResult.message || `已入库 ${confirmResult.imported_rows} 行` }}
        </t-alert>
        <div class="step-actions">
          <t-button theme="default" @click="restart">返回首页</t-button>
          <t-button
            v-privilege="[PERM.impexpImportConfirm]"
            theme="primary"
            :loading="confirming"
            :disabled="!!confirmResult"
            @click="handleConfirm"
          >
            确认入库
          </t-button>
          <t-button theme="default" @click="goTaskList">查看任务列表</t-button>
        </div>
      </div>
    </t-card>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { useRouter } from 'vue-router';
import { MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import {
  confirmImport,
  downloadImportErrors,
  downloadImportTemplate,
  listDefinitions,
  uploadImportFile,
  type DefinitionBrief,
  type ImportConfirmRes,
  type ImportMode,
  type PrecheckReport,
} from '@/api/business/impexp';
import { formatFileSize } from '@/utils/format';
import { saveBlob } from '@/utils/download';
import { PERM } from '@/constants/permissions';

const router = useRouter();

/** 当前步骤：0 上传 / 1 校验报告 / 2 确认入库 */
const step = ref(0);
const defs = ref<DefinitionBrief[]>([]);
const defLoading = ref(false);
const bizType = ref('');
const mode = ref<ImportMode>('STRICT');
const pickedFile = ref<File | null>(null);
const fileInputRef = ref<HTMLInputElement>();
const dragging = ref(false);
const uploading = ref(false);
const confirming = ref(false);
const stepError = ref('');
const report = ref<PrecheckReport | null>(null);
const taskId = ref('');
const taskNo = ref('');
const confirmResult = ref<ImportConfirmRes | null>(null);

const bizOptions = computed(() =>
  defs.value
    .filter((item) => item.supports_import)
    .map((item) => ({ label: item.biz_name, value: item.biz_type })),
);

const currentDef = computed(() => defs.value.find((item) => item.biz_type === bizType.value));

/** 严格模式下有失败行不能入库；部分导入模式允许 */
const canConfirm = computed(() => {
  if (!report.value) {
    return false;
  }
  if (report.value.valid === 0) {
    return false;
  }
  return mode.value === 'PARTIAL' || report.value.error === 0;
});

const errorColumns: PrimaryTableCol[] = [
  { colKey: 'row_no', title: '行号', width: 100 },
  { colKey: 'column', title: '列', width: 160 },
  { colKey: 'raw_value', title: '原始值', width: 200, ellipsis: true },
  { colKey: 'error_message', title: '失败原因', width: 320 },
];

async function loadDefs(): Promise<void> {
  defLoading.value = true;
  try {
    defs.value = await listDefinitions();
    if (!bizType.value && bizOptions.value.length > 0) {
      bizType.value = bizOptions.value[0].value;
    }
  } catch (error) {
    showError(error);
  } finally {
    defLoading.value = false;
  }
}

function onBizChange(): void {
  pickedFile.value = null;
  stepError.value = '';
  if (currentDef.value && !currentDef.value.allow_partial) {
    mode.value = 'STRICT';
  }
}

function pickFile(): void {
  fileInputRef.value?.click();
}

function onFilePicked(event: Event): void {
  const target = event.target as HTMLInputElement;
  const file = target.files?.[0];
  target.value = '';
  if (file) {
    pickedFile.value = file;
  }
}

function onDrop(event: DragEvent): void {
  dragging.value = false;
  const file = event.dataTransfer?.files?.[0];
  if (file) {
    pickedFile.value = file;
  }
}

async function startUpload(): Promise<void> {
  if (!pickedFile.value || !bizType.value) {
    return;
  }
  uploading.value = true;
  stepError.value = '';
  try {
    const res = await uploadImportFile(pickedFile.value, bizType.value, mode.value);
    taskId.value = res.task_id;
    taskNo.value = res.task_no;
    report.value = res.report;
    step.value = 1;
  } catch (error) {
    stepError.value = error instanceof ApiError ? error.message : '上传失败，请稍后重试';
  } finally {
    uploading.value = false;
  }
}

async function handleDownloadTemplate(): Promise<void> {
  if (!bizType.value) {
    return;
  }
  try {
    const blob = await downloadImportTemplate(bizType.value);
    saveBlob(blob, `${currentDef.value?.biz_name || bizType.value}-导入模板.xlsx`);
  } catch (error) {
    showError(error);
  }
}

async function handleDownloadErrors(): Promise<void> {
  try {
    const blob = await downloadImportErrors(taskId.value);
    saveBlob(blob, `${taskNo.value}-错误行.xlsx`);
  } catch (error) {
    showError(error);
  }
}

function goConfirm(): void {
  step.value = 2;
}

async function handleConfirm(): Promise<void> {
  confirming.value = true;
  try {
    confirmResult.value = await confirmImport(taskId.value);
    void MessagePlugin.success(confirmResult.value.message || '已提交入库');
  } catch (error) {
    showError(error);
  } finally {
    confirming.value = false;
  }
}

function restart(): void {
  step.value = 0;
  pickedFile.value = null;
  report.value = null;
  taskId.value = '';
  taskNo.value = '';
  confirmResult.value = null;
  stepError.value = '';
}

function goTaskList(): void {
  void router.push('/business/impexp/import-task');
}

function showError(error: unknown): void {
  void MessagePlugin.error(error instanceof ApiError ? error.message : '请求失败，请稍后重试');
}

onMounted(() => {
  void loadDefs();
});
</script>

<style lang="less" scoped>
.import-wizard-page {
  padding: 16px;

  .wizard-steps {
    margin: 8px 0 24px;
  }

  .step-body {
    padding: 0 8px 8px;
  }

  .step-form {
    max-width: 720px;

    &__select {
      width: 320px;
    }
  }

  .hint {
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }

  .drop-zone {
    margin: 16px 0;
    padding: 40px 0;
    text-align: center;
    border: 1px dashed var(--td-component-border);
    border-radius: 8px;
    background: var(--td-bg-color-page);
    transition: border-color 0.2s;

    &--active {
      border-color: var(--td-brand-color-7);
    }

    &__icon {
      font-size: 44px;
      color: var(--td-text-color-placeholder);
    }

    &__title {
      margin: 12px 0;
      color: var(--td-text-color-primary);
    }

    &__desc {
      margin-top: 8px;
      font-size: 12px;
      color: var(--td-text-color-secondary);
    }
  }

  .file-input {
    display: none;
  }

  .picked {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px 16px;
    border-radius: 6px;
    background: var(--td-bg-color-secondarycontainer);

    &__name {
      font-weight: 600;
    }

    &__size {
      color: var(--td-text-color-secondary);
      font-size: 12px;
    }
  }

  .summary {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
    margin-bottom: 16px;

    &__item {
      min-width: 160px;
      padding: 12px 16px;
      border: 1px solid var(--td-component-stroke);
      border-radius: 6px;

      &--ok {
        border-color: var(--td-success-color);
      }

      &--error {
        border-color: var(--td-error-color);
      }
    }

    &__label {
      display: block;
      font-size: 12px;
      color: var(--td-text-color-secondary);
    }

    &__value {
      font-size: 20px;
      font-weight: 600;
      color: var(--td-text-color-primary);
    }
  }

  .step-alert {
    margin-bottom: 12px;
  }

  .fail-table {
    margin-top: 8px;

    &__head {
      display: flex;
      align-items: center;
      justify-content: space-between;
      margin-bottom: 8px;
      font-weight: 600;
    }
  }

  .cell-mono {
    font-family: var(--app-font-mono, monospace);
    font-size: 12px;
  }

  .step-actions {
    display: flex;
    justify-content: flex-end;
    gap: 12px;
    margin-top: 20px;
    padding-top: 16px;
    border-top: 1px solid var(--td-component-stroke);
  }
}
</style>

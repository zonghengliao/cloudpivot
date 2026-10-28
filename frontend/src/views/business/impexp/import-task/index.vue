<template>
  <div class="import-task-page">
    <t-card :bordered="false" class="filter-card">
      <t-row :gutter="16">
        <t-col :span="3">
          <t-input v-model="query.keyword" placeholder="任务号 / 文件名" clearable @enter="handleSearch" />
        </t-col>
        <t-col :span="3">
          <t-select v-model="query.status" placeholder="状态" clearable>
            <t-option v-for="item in STATUS_OPTIONS" :key="item.value" :value="item.value" :label="item.label" />
          </t-select>
        </t-col>
        <t-col :span="3">
          <t-checkbox v-model="query.only_mine">只看我的</t-checkbox>
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
          <t-button v-privilege="[PERM.impexpImportUpload]" theme="primary" @click="goWizard">
            <template #icon><t-icon name="upload" /></template>
            发起导入
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
        <template #task_no="{ row }">
          <t-link theme="primary" @click="openDetail(row)">{{ row.task_no }}</t-link>
        </template>
        <template #biz_type="{ row }">{{ row.biz_name || row.biz_type }}</template>
        <template #status="{ row }">
          <t-tag :theme="statusTheme(row.status)" variant="light" size="small">{{ statusLabel(row.status) }}</t-tag>
        </template>
        <template #progress="{ row }">
          <t-progress
            :percentage="row.progress"
            :status="row.status === 'FAILED' ? 'error' : row.progress === 100 ? 'success' : 'active'"
            :label="true"
            size="small"
          />
        </template>
        <template #rows="{ row }">
          <span>{{ row.valid_rows }} / {{ row.total_rows }}</span>
          <t-tooltip v-if="row.error_rows > 0" :content="`失败 ${row.error_rows} 行`">
            <t-tag class="cell-tag" theme="danger" size="small" variant="light">{{ row.error_rows }} 失败</t-tag>
          </t-tooltip>
        </template>
        <template #created_at="{ row }">{{ formatDateTime(row.created_at) }}</template>
        <template #op="{ row }">
          <t-space size="small">
            <t-link theme="primary" @click="openDetail(row)">详情</t-link>
            <t-link
              v-if="row.status === 'VALIDATED' && row.error_rows === 0"
              v-privilege="[PERM.impexpImportConfirm]"
              theme="primary"
              @click="handleConfirm(row)"
            >
              确认入库
            </t-link>
            <t-link
              v-if="row.status === 'PENDING' || row.status === 'VALIDATED'"
              v-privilege="[PERM.impexpImportCancel]"
              theme="warning"
              @click="confirmCancel(row)"
            >
              取消
            </t-link>
            <t-link
              v-if="row.status === 'FAILED'"
              v-privilege="[PERM.impexpImportRetry]"
              theme="primary"
              @click="handleRetry(row)"
            >
              重试
            </t-link>
            <t-link
              v-if="row.error_rows > 0"
              v-privilege="[PERM.impexpImportDownload]"
              theme="primary"
              @click="downloadErrors(row)"
            >
              错误明细
            </t-link>
          </t-space>
        </template>
        <template #empty>
          <div class="empty-state">
            <t-icon class="empty-state__icon" name="upload" />
            <p class="empty-state__title">还没有导入任务</p>
            <p class="empty-state__desc">从「发起导入」上传 Excel，校验通过后确认入库</p>
            <t-button v-privilege="[PERM.impexpImportUpload]" theme="primary" @click="goWizard">发起导入</t-button>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- 详情抽屉 -->
    <t-drawer v-model:visible="detailVisible" :header="`导入任务 ${detail?.task_no ?? ''}`" size="780px" :footer="false">
      <template v-if="detail">
        <t-descriptions :column="2" bordered size="small">
          <t-descriptions-item label="状态">
            <t-tag :theme="statusTheme(detail.status)" variant="light" size="small">{{ statusLabel(detail.status) }}</t-tag>
          </t-descriptions-item>
          <t-descriptions-item label="业务">{{ detail.biz_name || detail.biz_type }}</t-descriptions-item>
          <t-descriptions-item label="文件">{{ detail.file_name || '—' }}</t-descriptions-item>
          <t-descriptions-item label="大小">{{ formatFileSize(detail.file_size) }}</t-descriptions-item>
          <t-descriptions-item label="总行数">{{ detail.total_rows.toLocaleString() }}</t-descriptions-item>
          <t-descriptions-item label="通过 / 失败">
            {{ detail.valid_rows }} / {{ detail.error_rows }}
          </t-descriptions-item>
          <t-descriptions-item label="已入库">{{ detail.imported_rows }}</t-descriptions-item>
          <t-descriptions-item label="创建时间">{{ formatDateTime(detail.created_at) }}</t-descriptions-item>
          <t-descriptions-item label="结果说明" :span="2">{{ detail.message || '—' }}</t-descriptions-item>
        </t-descriptions>

        <div class="detail-errors">
          <div class="detail-errors__head">
            <span>失败明细</span>
            <t-button
              v-if="detail.error_rows > 0"
              v-privilege="[PERM.impexpImportDownload]"
              size="small"
              variant="outline"
              @click="downloadErrors(detail)"
            >
              下载错误明细
            </t-button>
          </div>
          <t-table
            row-key="row_no"
            :data="errorRows"
            :columns="errorColumns"
            :loading="errorLoading"
            :pagination="errorPagination"
            size="small"
            :stripe="true"
            @page-change="onErrorPageChange"
          >
            <template #row_no="{ row }">第 {{ row.row_no }} 行</template>
            <template #column="{ row }">{{ row.column_header || row.column_key || '—' }}</template>
            <template #error_message="{ row }">{{ row.error_message }}</template>
          </t-table>
        </div>
      </template>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
import { onBeforeUnmount, onMounted, reactive, ref } from 'vue';
import { useRouter } from 'vue-router';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import {
  cancelImport,
  confirmImport,
  downloadImportErrors,
  getImportTask,
  pageImportErrors,
  pageImportTasks,
  retryImport,
  type ImportErrorRes,
  type ImportTaskRes,
  type ImportTaskStatus,
} from '@/api/business/impexp';
import { formatDateTime, formatFileSize } from '@/utils/format';
import { saveBlob } from '@/utils/download';
import { PERM } from '@/constants/permissions';

/** 表格分页事件载荷 */
interface TablePageInfo {
  /** 当前页码 */
  current: number;
  /** 每页条数 */
  pageSize: number;
}

const router = useRouter();
const rows = ref<ImportTaskRes[]>([]);
const loading = ref(false);
const query = reactive({ keyword: '', status: '' as string, only_mine: false });
const pagination = reactive({ current: 1, pageSize: 10, total: 0 });

/** 状态筛选项（与后端状态机逐字对齐） */
const STATUS_OPTIONS: Array<{ value: ImportTaskStatus; label: string }> = [
  { value: 'PENDING', label: '待校验' },
  { value: 'VALIDATING', label: '校验中' },
  { value: 'VALIDATED', label: '待确认' },
  { value: 'IMPORTING', label: '入库中' },
  { value: 'SUCCESS', label: '成功' },
  { value: 'PARTIAL', label: '部分成功' },
  { value: 'FAILED', label: '失败' },
  { value: 'CANCELLED', label: '已取消' },
];

/** 状态 → 中文（8.0 约定第 3 条：状态色语义固定） */
function statusLabel(status: ImportTaskStatus): string {
  return STATUS_OPTIONS.find((item) => item.value === status)?.label ?? status;
}

/** 状态 → 主题色 */
function statusTheme(status: ImportTaskStatus): 'success' | 'primary' | 'warning' | 'danger' | 'default' {
  if (status === 'SUCCESS') {
    return 'success';
  }
  if (status === 'PARTIAL') {
    return 'warning';
  }
  if (status === 'FAILED') {
    return 'danger';
  }
  if (status === 'CANCELLED') {
    return 'default';
  }
  return 'primary';
}

const columns: PrimaryTableCol[] = [
  { colKey: 'task_no', title: '任务号', width: 190 },
  { colKey: 'biz_type', title: '业务', width: 150 },
  { colKey: 'status', title: '状态', width: 110 },
  { colKey: 'progress', title: '进度', width: 160 },
  { colKey: 'rows', title: '通过 / 总数', width: 160 },
  { colKey: 'created_at', title: '创建时间', width: 170 },
  { colKey: 'op', title: '操作', width: 260, fixed: 'right' },
];

const errorColumns: PrimaryTableCol[] = [
  { colKey: 'row_no', title: '行号', width: 100 },
  { colKey: 'column', title: '列', width: 150 },
  { colKey: 'error_message', title: '失败原因', width: 360 },
];

async function loadList(): Promise<void> {
  loading.value = true;
  try {
    const data = await pageImportTasks({
      page: pagination.current,
      page_size: pagination.pageSize,
      keyword: query.keyword || undefined,
      status: query.status || undefined,
      only_mine: query.only_mine || undefined,
    });
    rows.value = data.list;
    pagination.total = data.total;
    schedulePolling();
  } catch (error) {
    showError(error);
  } finally {
    loading.value = false;
  }
}

/** 有进行中的任务时 3 秒轮询一次进度，全部终态后自动停表 */
let pollTimer: number | undefined;
function schedulePolling(): void {
  const hasActive = rows.value.some((row) => ['PENDING', 'VALIDATING', 'IMPORTING'].includes(row.status));
  if (pollTimer) {
    window.clearTimeout(pollTimer);
    pollTimer = undefined;
  }
  if (!hasActive) {
    return;
  }
  pollTimer = window.setTimeout(() => {
    void loadList();
  }, 3000);
}

function handleSearch(): void {
  pagination.current = 1;
  void loadList();
}

function handleReset(): void {
  query.keyword = '';
  query.status = '';
  query.only_mine = false;
  handleSearch();
}

function handlePageChange(pageInfo: TablePageInfo): void {
  pagination.current = pageInfo.current;
  pagination.pageSize = pageInfo.pageSize;
  void loadList();
}

// ==================== 详情 / 错误明细 ====================
const detailVisible = ref(false);
const detail = ref<ImportTaskRes | null>(null);
const errorRows = ref<ImportErrorRes[]>([]);
const errorLoading = ref(false);
const errorPagination = reactive({ current: 1, pageSize: 10, total: 0 });

async function openDetail(row: ImportTaskRes): Promise<void> {
  detailVisible.value = true;
  errorPagination.current = 1;
  try {
    detail.value = await getImportTask(row.id);
    await loadErrors();
  } catch (error) {
    showError(error);
  }
}

async function loadErrors(): Promise<void> {
  if (!detail.value) {
    return;
  }
  errorLoading.value = true;
  try {
    const data = await pageImportErrors(detail.value.id, errorPagination.current, errorPagination.pageSize);
    errorRows.value = data.list;
    errorPagination.total = data.total;
  } catch (error) {
    showError(error);
  } finally {
    errorLoading.value = false;
  }
}

function onErrorPageChange(pageInfo: TablePageInfo): void {
  errorPagination.current = pageInfo.current;
  errorPagination.pageSize = pageInfo.pageSize;
  void loadErrors();
}

// ==================== 行内操作 ====================
function handleConfirm(row: ImportTaskRes): void {
  const dialog = DialogPlugin.confirm({
    header: '确认入库',
    body: `即将把 ${row.valid_rows} 行数据写入业务表，不可撤回。是否继续？`,
    confirmBtn: { content: '确认入库', theme: 'primary' },
    onConfirm: () => {
      dialog.destroy();
      void (async () => {
        try {
          const res = await confirmImport(row.id);
          void MessagePlugin.success(res.message || `已入库 ${res.imported_rows} 行`);
          await loadList();
        } catch (error) {
          showError(error);
        }
      })();
    },
  });
}

function confirmCancel(row: ImportTaskRes): void {
  const dialog = DialogPlugin.confirm({
    header: '取消导入',
    body: `确定取消任务 ${row.task_no}？已上传的文件不会被删除。`,
    theme: 'warning',
    confirmBtn: { content: '取消任务', theme: 'warning' },
    onConfirm: () => {
      dialog.destroy();
      void (async () => {
        try {
          await cancelImport(row.id);
          void MessagePlugin.success('任务已取消');
          await loadList();
        } catch (error) {
          showError(error);
        }
      })();
    },
  });
}

async function handleRetry(row: ImportTaskRes): Promise<void> {
  try {
    const created = await retryImport(row.id);
    void MessagePlugin.success(`已创建重试任务 ${created.task_no}`);
    await loadList();
  } catch (error) {
    showError(error);
  }
}

async function downloadErrors(row: ImportTaskRes): Promise<void> {
  try {
    const blob = await downloadImportErrors(row.id);
    saveBlob(blob, `${row.task_no}-错误行.xlsx`);
  } catch (error) {
    showError(error);
  }
}

function goWizard(): void {
  void router.push('/business/impexp/import');
}

function showError(error: unknown): void {
  void MessagePlugin.error(error instanceof ApiError ? error.message : '请求失败，请稍后重试');
}

onMounted(() => {
  void loadList();
});

onBeforeUnmount(() => {
  if (pollTimer) {
    window.clearTimeout(pollTimer);
  }
});
</script>

<style lang="less" scoped>
.import-task-page {
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

  .cell-tag {
    margin-left: 6px;
  }

  .detail-errors {
    margin-top: 16px;

    &__head {
      display: flex;
      align-items: center;
      justify-content: space-between;
      margin-bottom: 8px;
      font-weight: 600;
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

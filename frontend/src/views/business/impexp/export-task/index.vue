<template>
  <div class="export-task-page">
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
          <t-button v-privilege="[PERM.impexpExportCreate]" theme="primary" @click="openCreate">
            <template #icon><t-icon name="download" /></template>
            新建导出
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
        <template #task_no="{ row }">{{ row.task_no }}</template>
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
        <template #rows="{ row }">{{ (row.exported_rows || 0).toLocaleString() }} / {{ (row.total_rows || 0).toLocaleString() }}</template>
        <template #expire_at="{ row }">
          <span v-if="row.status === 'SUCCESS' && row.expire_at" :class="{ 'cell-expired': isExpired(row.expire_at) }">
            {{ expireText(row.expire_at) }}
          </span>
          <span v-else class="cell-empty">—</span>
        </template>
        <template #created_at="{ row }">{{ formatDateTime(row.created_at) }}</template>
        <template #op="{ row }">
          <t-space size="small">
            <t-link
              v-if="row.status === 'SUCCESS'"
              v-privilege="[PERM.impexpExportDownload]"
              :theme="isExpired(row.expire_at) ? 'default' : 'primary'"
              :disabled="isExpired(row.expire_at)"
              @click="handleDownload(row)"
            >
              下载
            </t-link>
            <t-link
              v-if="row.status === 'PENDING' || row.status === 'RUNNING'"
              v-privilege="[PERM.impexpExportCancel]"
              theme="warning"
              @click="confirmCancel(row)"
            >
              取消
            </t-link>
            <t-link
              v-if="canRetry(row)"
              v-privilege="[PERM.impexpExportRetry]"
              theme="primary"
              @click="confirmRetry(row)"
            >
              重试
            </t-link>
            <t-link v-privilege="[PERM.impexpExportDelete]" theme="danger" @click="confirmRemove(row)">删除</t-link>
          </t-space>
        </template>
        <template #empty>
          <div class="empty-state">
            <t-icon class="empty-state__icon" name="download" />
            <p class="empty-state__title">还没有导出任务</p>
            <p class="empty-state__desc">数据量大时会转为后台导出，完成后可在本页下载（链接 24 小时内有效）</p>
            <t-button v-privilege="[PERM.impexpExportCreate]" theme="primary" @click="openCreate">新建导出</t-button>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- 新建导出 -->
    <t-dialog
      v-model:visible="createVisible"
      header="新建导出"
      :confirm-btn="{ content: '开始导出', theme: 'primary', loading: creating }"
      width="520px"
      destroy-on-close
      @confirm="submitCreate"
    >
      <t-form label-width="96px">
        <t-form-item label="导出业务">
          <t-select v-model="createBizType" :options="bizOptions" :loading="defLoading" placeholder="请选择业务" />
        </t-form-item>
        <t-form-item label="说明">
          <span class="hint">
            数据量超过同步阈值（5,000 行）时自动转后台导出，完成后可在任务列表下载；导出结果按当前筛选条件生成。
          </span>
        </t-form-item>
      </t-form>
      <t-alert v-if="createError" theme="error" :message="createError" />
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import dayjs from 'dayjs';
import { ApiError } from '@/api/request';
import {
  cancelExport,
  createExport,
  deleteExport,
  downloadExport,
  listDefinitions,
  pageExportTasks,
  retryExport,
  type DefinitionBrief,
  type ExportTaskRes,
  type ExportTaskStatus,
} from '@/api/business/impexp';
import { formatDateTime } from '@/utils/format';
import { saveBlob } from '@/utils/download';
import { PERM } from '@/constants/permissions';

/** 表格分页事件载荷 */
interface TablePageInfo {
  /** 当前页码 */
  current: number;
  /** 每页条数 */
  pageSize: number;
}

const rows = ref<ExportTaskRes[]>([]);
const loading = ref(false);
const query = reactive({ keyword: '', status: '' as string });
const pagination = reactive({ current: 1, pageSize: 10, total: 0 });

const STATUS_OPTIONS: Array<{ value: ExportTaskStatus; label: string }> = [
  { value: 'PENDING', label: '排队中' },
  { value: 'RUNNING', label: '导出中' },
  { value: 'SUCCESS', label: '已完成' },
  { value: 'FAILED', label: '失败' },
  { value: 'CANCELLED', label: '已取消' },
];

function statusLabel(status: ExportTaskStatus): string {
  return STATUS_OPTIONS.find((item) => item.value === status)?.label ?? status;
}

function statusTheme(status: ExportTaskStatus): 'success' | 'primary' | 'warning' | 'danger' | 'default' {
  if (status === 'SUCCESS') {
    return 'success';
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
  { colKey: 'rows', title: '已导出 / 总行数', width: 170 },
  { colKey: 'expire_at', title: '下载有效期', width: 200 },
  { colKey: 'created_at', title: '创建时间', width: 170 },
  { colKey: 'op', title: '操作', width: 200, fixed: 'right' },
];

/** 下载链接是否已过期（E-EXP-05：有效期 24 小时） */
function isExpired(expireAt?: string | null): boolean {
  if (!expireAt) {
    return false;
  }
  return dayjs(expireAt).isBefore(dayjs());
}

/**
 * 是否可重试：失败 / 已取消 / 产物已过期（后端 `requeue_export_task` 的三种前置状态）。
 * 与后端状态机保持一致，避免出现「按钮能点但必然 409」。
 */
function canRetry(row: ExportTaskRes): boolean {
  if (row.status === 'FAILED' || row.status === 'CANCELLED') {
    return true;
  }
  return row.status === 'SUCCESS' && isExpired(row.expire_at);
}

/** 有效期文案：剩余时间比绝对时间更好读 */
function expireText(expireAt: string): string {
  if (isExpired(expireAt)) {
    return '已过期，请重新导出';
  }
  const hours = dayjs(expireAt).diff(dayjs(), 'hour');
  if (hours >= 1) {
    return `${dayjs(expireAt).format('MM-DD HH:mm')}（剩 ${hours} 小时）`;
  }
  const minutes = Math.max(1, dayjs(expireAt).diff(dayjs(), 'minute'));
  return `${dayjs(expireAt).format('HH:mm')}（剩 ${minutes} 分钟）`;
}

async function loadList(): Promise<void> {
  loading.value = true;
  try {
    const data = await pageExportTasks({
      page: pagination.current,
      page_size: pagination.pageSize,
      keyword: query.keyword || undefined,
      status: query.status || undefined,
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

/** 有未完成任务时 3 秒轮询一次（架构文档 §5.3 第 5 条） */
let pollTimer: number | undefined;
function schedulePolling(): void {
  if (pollTimer) {
    window.clearTimeout(pollTimer);
    pollTimer = undefined;
  }
  const hasActive = rows.value.some((row) => row.status === 'PENDING' || row.status === 'RUNNING');
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
  handleSearch();
}

function handlePageChange(pageInfo: TablePageInfo): void {
  pagination.current = pageInfo.current;
  pagination.pageSize = pageInfo.pageSize;
  void loadList();
}

// ==================== 新建导出 ====================
const createVisible = ref(false);
const creating = ref(false);
const createError = ref('');
const createBizType = ref('');
const defs = ref<DefinitionBrief[]>([]);
const defLoading = ref(false);
const bizOptions = computed(() =>
  defs.value.filter((item) => item.supports_export).map((item) => ({ label: item.biz_name, value: item.biz_type })),
);

async function openCreate(): Promise<void> {
  createVisible.value = true;
  createError.value = '';
  if (defs.value.length === 0) {
    defLoading.value = true;
    try {
      defs.value = await listDefinitions();
      if (!createBizType.value && bizOptions.value.length > 0) {
        createBizType.value = bizOptions.value[0].value;
      }
    } catch (error) {
      showError(error);
    } finally {
      defLoading.value = false;
    }
  }
}

async function submitCreate(): Promise<void> {
  if (!createBizType.value) {
    createError.value = '请选择导出业务';
    return;
  }
  creating.value = true;
  createError.value = '';
  try {
    const task = await createExport({ biz_type: createBizType.value });
    void MessagePlugin.success(`导出任务 ${task.task_no} 已创建，完成后会收到站内信通知`);
    createVisible.value = false;
    pagination.current = 1;
    await loadList();
  } catch (error) {
    createError.value = error instanceof ApiError ? error.message : '创建导出任务失败';
  } finally {
    creating.value = false;
  }
}

// ==================== 行内操作 ====================
async function handleDownload(row: ExportTaskRes): Promise<void> {
  try {
    const blob = await downloadExport(row.id);
    saveBlob(blob, row.file_name || `${row.task_no}.xlsx`);
  } catch (error) {
    showError(error);
  }
}

function confirmCancel(row: ExportTaskRes): void {
  const dialog = DialogPlugin.confirm({
    header: '取消导出',
    body: `确定取消任务 ${row.task_no}？`,
    theme: 'warning',
    confirmBtn: { content: '取消任务', theme: 'warning' },
    onConfirm: () => {
      dialog.destroy();
      void (async () => {
        try {
          await cancelExport(row.id);
          void MessagePlugin.success('任务已取消');
          await loadList();
        } catch (error) {
          showError(error);
        }
      })();
    },
  });
}

function confirmRetry(row: ExportTaskRes): void {
  const dialog = DialogPlugin.confirm({
    header: '重试导出',
    body: `确定重新执行任务 ${row.task_no}？将复用同一条任务记录重新排队。`,
    theme: 'info',
    confirmBtn: { content: '重新执行', theme: 'primary' },
    onConfirm: () => {
      dialog.destroy();
      void (async () => {
        try {
          await retryExport(row.id);
          void MessagePlugin.success('已重新排队，完成后可在本页下载');
          await loadList();
        } catch (error) {
          showError(error);
        }
      })();
    },
  });
}

function confirmRemove(row: ExportTaskRes): void {
  const dialog = DialogPlugin.confirm({
    header: '删除导出任务',
    body: `确定删除任务 ${row.task_no} 及其产物文件？删除后不可恢复。`,
    theme: 'danger',
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: () => {
      dialog.destroy();
      void (async () => {
        try {
          await deleteExport(row.id);
          void MessagePlugin.success('已删除');
          await loadList();
        } catch (error) {
          showError(error);
        }
      })();
    },
  });
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
.export-task-page {
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

  .cell-empty {
    color: var(--td-text-color-placeholder);
  }

  .cell-expired {
    color: var(--td-error-color);
  }

  .hint {
    font-size: 12px;
    line-height: 1.6;
    color: var(--td-text-color-secondary);
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

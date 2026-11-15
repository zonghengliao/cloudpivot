<!--
  导出任务列表（impexp）。

  # 列表部分（P3-11）
  筛选 / 分页 / 空态 / 加载失败交给 `components/ListPage`：本页只声明
  「列 + 筛选项 + 怎么拉一页 + 入参怎么拼」。

  # 3 秒轮询（组件不管，页面自己挂）
  `PENDING / RUNNING` 的导出进度是后台任务推进的（架构文档 §5.3 第 5 条），页面靠
  `@loaded` 拿到这一页，只有存在未完成的行才安排下一跳；全部终态后自动停表。
  轮询只重拉当前页，不重置筛选、不跳页。

  # 两种「下载不了」的判定（都按后端状态机写，避免「按钮能点但必然 409/410」）
  - `canRetry`：失败 / 已取消 / **产物已过期**（后端 `requeue_export_task` 的三种前置状态）；
  - 下载链接：`SUCCESS` 才有，过期后置灰（E-EXP-05 24 小时有效期）。
-->
<template>
  <div class="export-task-page">
    <ListPage
      ref="listRef"
      title="导出任务"
      testid="impexp-export"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      :page-size="PAGE_SIZE"
      @loaded="onLoaded"
    >
      <template #toolbar>
        <t-button
          v-privilege="[PERM.impexpExportCreate]"
          theme="primary"
          data-testid="impexp-export-create"
          @click="openCreate"
        >
          <template #icon><t-icon name="download" /></template>
          新建导出
        </t-button>
      </template>

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
      <template #rows="{ row }">
        {{ (row.exported_rows || 0).toLocaleString() }} / {{ (row.total_rows || 0).toLocaleString() }}
      </template>
      <template #expire_at="{ row }">
        <span v-if="row.status === 'SUCCESS' && row.expire_at" :class="{ 'cell-expired': isExpired(row.expire_at) }">
          {{ expireText(row.expire_at) }}
        </span>
        <span v-else class="cell-empty">—</span>
      </template>
      <template #created_at="{ row }">{{ formatDateTime(row.created_at) }}</template>

      <!-- 行操作按状态出现 → 页面自绘（声明式行操作只能整条置灰，不能按行隐藏） -->
      <template #op="{ row }">
        <t-space size="small">
          <t-link
            v-if="row.status === 'SUCCESS'"
            v-privilege="[PERM.impexpExportDownload]"
            :theme="isExpired(row.expire_at) ? 'default' : 'primary'"
            :disabled="isExpired(row.expire_at)"
            data-testid="impexp-export-download"
            @click="handleDownload(row)"
          >
            下载
          </t-link>
          <t-link
            v-if="row.status === 'PENDING' || row.status === 'RUNNING'"
            v-privilege="[PERM.impexpExportCancel]"
            theme="warning"
            data-testid="impexp-export-cancel"
            @click="confirmCancel(row)"
          >
            取消
          </t-link>
          <t-link
            v-if="canRetry(row)"
            v-privilege="[PERM.impexpExportRetry]"
            theme="primary"
            data-testid="impexp-export-retry"
            @click="confirmRetry(row)"
          >
            重试
          </t-link>
          <t-link
            v-privilege="[PERM.impexpExportDelete]"
            theme="danger"
            data-testid="impexp-export-delete"
            @click="confirmRemove(row)"
          >
            删除
          </t-link>
        </t-space>
      </template>

      <template #empty="{ filtered }">
        <div class="panel-empty panel-empty--file" data-testid="impexp-export-empty">
          <t-icon class="panel-empty__icon" name="download" />
          <p class="panel-empty__title">{{ filtered ? '没有匹配的导出任务' : '还没有导出任务' }}</p>
          <p class="panel-empty__desc">
            {{
              filtered
                ? '换个关键词 / 状态试试，或点「重置」看全部'
                : '数据量大时会转为后台导出，完成后可在本页下载（链接 24 小时内有效）'
            }}
          </p>
          <t-button v-privilege="[PERM.impexpExportCreate]" theme="primary" @click="openCreate">新建导出</t-button>
        </div>
      </template>
    </ListPage>

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
          <t-select
            v-model="createBizType"
            :options="bizOptions"
            :loading="defLoading"
            placeholder="请选择业务"
            data-testid="impexp-export-biz"
          />
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
import { computed, onBeforeUnmount, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';
import dayjs from 'dayjs';
import ListPage from '@/components/ListPage/index.vue';
import type { ColumnDef, FilterDef, ListFetcher, ListFilter, ListQueryBuilder } from '@/components/ListPage/types';
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

/** 每页条数（与改造前一致） */
const PAGE_SIZE = 10;

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

// ==================== 列表（P3-11） ====================

const filter = reactive<ListFilter>({ keyword: '', status: '' });

const filters: FilterDef[] = [
  { key: 'keyword', type: 'keyword', placeholder: '任务号 / 文件名', width: 260, testid: 'impexp-export-keyword' },
  {
    key: 'status',
    type: 'select',
    placeholder: '状态',
    width: 160,
    options: STATUS_OPTIONS,
    testid: 'impexp-export-status',
  },
];

const columns: ColumnDef[] = [
  { colKey: 'task_no', title: '任务号', width: 190 },
  { colKey: 'biz_type', title: '业务', width: 150 },
  { colKey: 'status', title: '状态', width: 110 },
  { colKey: 'progress', title: '进度', width: 160 },
  { colKey: 'rows', title: '已导出 / 总行数', width: 170, exportable: false },
  { colKey: 'expire_at', title: '下载有效期', width: 200 },
  { colKey: 'created_at', title: '创建时间', width: 170 },
  { colKey: 'op', title: '操作', width: 200, fixed: 'right', exportable: false },
];

const fetcher: ListFetcher<ExportTaskRes> = (query) => pageExportTasks(query as Parameters<typeof pageExportTasks>[0]);

/** 入参拼装：空串整键缺席 */
const queryBuilder: ListQueryBuilder = (_state, page, size) => {
  const keyword = String(filter.keyword ?? '').trim();
  return {
    page,
    page_size: size,
    keyword: keyword || undefined,
    status: filter.status || undefined,
  };
};

const listRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 一页拉完：安排轮询（只重拉当前页） */
function onLoaded(payload: { rows: ExportTaskRes[] }): void {
  schedulePolling(payload.rows);
}

/** 有未完成任务时 3 秒轮询一次（架构文档 §5.3 第 5 条） */
let pollTimer: number | undefined;
function schedulePolling(list: ExportTaskRes[]): void {
  if (pollTimer) {
    window.clearTimeout(pollTimer);
    pollTimer = undefined;
  }
  const hasActive = list.some((row) => row.status === 'PENDING' || row.status === 'RUNNING');
  if (!hasActive) {
    return;
  }
  pollTimer = window.setTimeout(() => {
    listRef.value?.reload();
  }, 3000);
}

/** 增删改后重拉当前页 */
function reloadList(): void {
  listRef.value?.reload();
}

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
    listRef.value?.search();
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
          reloadList();
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
          reloadList();
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
          reloadList();
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

onBeforeUnmount(() => {
  if (pollTimer) {
    window.clearTimeout(pollTimer);
  }
});
</script>

<style lang="less" scoped>
.export-task-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
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
</style>

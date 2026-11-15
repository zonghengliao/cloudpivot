<!--
  导入任务列表（impexp）。

  # 列表部分（P3-11）
  筛选 / 分页 / 空态 / 加载失败全部交给 `components/ListPage`：本页只声明
  「列 + 筛选项 + 怎么拉一页 + 入参怎么拼」。组件统一的口径（空串筛选整键缺席、
  删掉最后一页最后一条时回夹页码、失败红条带重试）不再在这里重复一份。

  # 3 秒轮询（组件不管，页面自己挂）
  `PENDING / VALIDATING / IMPORTING` 的进度是后台推进的，页面靠 `@loaded` 拿到这一页，
  只有存在「进行中」的行才安排下一跳，全部终态后自动停表（`schedulePolling`）。
  轮询只是「再拉一次当前页」，不重置筛选、不跳页 —— 用户正翻到第 2 页时不能被弹回第 1 页。

  # 「只看我的」为什么走 custom 槽
  ListPage 的声明式控件只有 keyword / select / number（`date-range` 与 `custom` 也要页面自绘），
  复选框属于页面自己的交互，用 `#filter-only_mine` 填。
-->
<template>
  <div class="import-task-page">
    <ListPage
      ref="listRef"
      title="导入任务"
      testid="impexp-import"
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
          v-privilege="[PERM.impexpImportUpload]"
          theme="primary"
          data-testid="impexp-import-create"
          @click="goWizard"
        >
          <template #icon><t-icon name="upload" /></template>
          发起导入
        </t-button>
      </template>

      <!-- 只看我的：复选框（ListPage 无此控件，走 custom 槽） -->
      <template #filter-only_mine>
        <t-checkbox v-model="filter.only_mine" data-testid="impexp-import-only-mine">只看我的</t-checkbox>
      </template>

      <template #task_no="{ row }">
        <t-link theme="primary" data-testid="impexp-import-open" @click="openDetail(row)">{{ row.task_no }}</t-link>
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

      <!--
        行操作保留页面自绘（`#op`）：这几条链接的出现条件取决于**行状态**，
        声明式行操作只能整条置灰、不能按行隐藏（见 types.ts `RowActionDef`）。
      -->
      <template #op="{ row }">
        <t-space size="small">
          <t-link theme="primary" data-testid="impexp-import-detail" @click="openDetail(row)">详情</t-link>
          <t-link
            v-if="row.status === 'VALIDATED' && row.error_rows === 0"
            v-privilege="[PERM.impexpImportConfirm]"
            theme="primary"
            data-testid="impexp-import-confirm"
            @click="handleConfirm(row)"
          >
            确认入库
          </t-link>
          <t-link
            v-if="row.status === 'PENDING' || row.status === 'VALIDATED'"
            v-privilege="[PERM.impexpImportCancel]"
            theme="warning"
            data-testid="impexp-import-cancel"
            @click="confirmCancel(row)"
          >
            取消
          </t-link>
          <t-link
            v-if="row.status === 'FAILED'"
            v-privilege="[PERM.impexpImportRetry]"
            theme="primary"
            data-testid="impexp-import-retry"
            @click="handleRetry(row)"
          >
            重试
          </t-link>
          <t-link
            v-if="row.error_rows > 0"
            v-privilege="[PERM.impexpImportDownload]"
            theme="primary"
            data-testid="impexp-import-errors"
            @click="downloadErrors(row)"
          >
            错误明细
          </t-link>
        </t-space>
      </template>

      <template #empty="{ filtered }">
        <div class="panel-empty panel-empty--file" data-testid="impexp-import-empty">
          <t-icon class="panel-empty__icon" name="upload" />
          <p class="panel-empty__title">{{ filtered ? '没有匹配的导入任务' : '还没有导入任务' }}</p>
          <p class="panel-empty__desc">
            {{
              filtered
                ? '换个关键词 / 状态试试，或点「重置」看全部'
                : '从「发起导入」上传 Excel，校验通过后确认入库'
            }}
          </p>
          <t-button v-privilege="[PERM.impexpImportUpload]" theme="primary" @click="goWizard">发起导入</t-button>
        </div>
      </template>
    </ListPage>

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
import { onBeforeUnmount, reactive, ref } from 'vue';
import { useRouter } from 'vue-router';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import ListPage from '@/components/ListPage/index.vue';
import type { ColumnDef, FilterDef, ListFetcher, ListFilter, ListQueryBuilder } from '@/components/ListPage/types';
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

/** 每页条数（与改造前一致） */
const PAGE_SIZE = 10;

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

// ==================== 列表（P3-11：声明交给 ListPage） ====================

const router = useRouter();
const filter = reactive<ListFilter>({ keyword: '', status: '', only_mine: false });

const filters: FilterDef[] = [
  { key: 'keyword', type: 'keyword', placeholder: '任务号 / 文件名', width: 260, testid: 'impexp-import-keyword' },
  {
    key: 'status',
    type: 'select',
    placeholder: '状态',
    width: 160,
    options: STATUS_OPTIONS,
    testid: 'impexp-import-status',
  },
  { key: 'only_mine', type: 'custom', defaultValue: false, testid: 'impexp-import-only-mine' },
];

const columns: ColumnDef[] = [
  { colKey: 'task_no', title: '任务号', width: 190 },
  { colKey: 'biz_type', title: '业务', width: 150 },
  { colKey: 'status', title: '状态', width: 110 },
  { colKey: 'progress', title: '进度', width: 160 },
  { colKey: 'rows', title: '通过 / 总数', width: 160, exportable: false },
  { colKey: 'created_at', title: '创建时间', width: 170 },
  { colKey: 'op', title: '操作', width: 260, fixed: 'right', exportable: false },
];

const errorColumns: PrimaryTableCol[] = [
  { colKey: 'row_no', title: '行号', width: 100 },
  { colKey: 'column', title: '列', width: 150 },
  { colKey: 'error_message', title: '失败原因', width: 360 },
];

const fetcher: ListFetcher<ImportTaskRes> = (query) => pageImportTasks(query as Parameters<typeof pageImportTasks>[0]);

/** 入参拼装：空串 / 未勾选整键缺席（`?only_mine=false` 与不传不是一回事） */
const queryBuilder: ListQueryBuilder = (_state, page, size) => ({
  page,
  page_size: size,
  keyword: textOrUndefined(filter.keyword),
  status: filter.status || undefined,
  only_mine: filter.only_mine ? true : undefined,
});

/** 去掉首尾空格，空串返回 undefined */
function textOrUndefined(value: unknown): string | undefined {
  const text = typeof value === 'string' ? value.trim() : '';
  return text || undefined;
}

const listRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 一页拉完：安排下一次轮询（轮询只重拉当前页，见 `schedulePolling`） */
function onLoaded(payload: { rows: ImportTaskRes[] }): void {
  schedulePolling(payload.rows);
}

/** 有进行中的任务时 3 秒轮询一次进度，全部终态后自动停表 */
let pollTimer: number | undefined;
function schedulePolling(list: ImportTaskRes[]): void {
  if (pollTimer) {
    window.clearTimeout(pollTimer);
    pollTimer = undefined;
  }
  const hasActive = list.some((row) => ['PENDING', 'VALIDATING', 'IMPORTING'].includes(row.status));
  if (!hasActive) {
    return;
  }
  pollTimer = window.setTimeout(() => {
    // 只重拉当前页：不重置筛选、不跳页（`reload` 走的就是 `load(current)`）
    listRef.value?.reload();
  }, 3000);
}

/** 增删改后重拉当前页 */
function reloadList(): void {
  listRef.value?.reload();
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
          reloadList();
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
          reloadList();
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
    reloadList();
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

onBeforeUnmount(() => {
  if (pollTimer) {
    window.clearTimeout(pollTimer);
  }
});
</script>

<style lang="less" scoped>
.import-task-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
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

</style>

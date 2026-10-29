<template>
  <div class="cl-list-page">
    <!-- 筛选 -->
    <t-card :bordered="false" class="filter-card">
      <t-row :gutter="[16, 12]">
        <t-col :span="4">
          <t-date-range-picker v-model="timeRange" placeholder="发生时间范围" clearable allow-input />
        </t-col>
        <t-col :span="2">
          <t-select v-model="query.module" placeholder="模块" clearable :options="moduleOptions" />
        </t-col>
        <t-col :span="3">
          <t-select v-model="query.resource" placeholder="资源表" clearable filterable :options="tableOptions" />
        </t-col>
        <t-col :span="2">
          <t-select v-model="query.action" placeholder="动作" clearable :options="actionOptions" />
        </t-col>
        <t-col :span="2">
          <t-input v-model="query.operator_name" placeholder="操作人" clearable @enter="handleSearch" />
        </t-col>
        <t-col :span="3">
          <t-input v-model="query.keyword" placeholder="关键字（动作 / 操作人 / 主键）" clearable @enter="handleSearch" />
        </t-col>
        <t-col :span="12">
          <t-space>
            <t-button theme="primary" @click="handleSearch">
              <template #icon><t-icon name="search" /></template>
              查询
            </t-button>
            <t-button variant="outline" @click="handleReset">重置</t-button>
            <t-button v-privilege="[PERM.changelogExport]" variant="outline" :loading="exporting" @click="handleExport">
              <template #icon><t-icon name="download" /></template>
              导出（脱敏 xlsx）
            </t-button>
          </t-space>
        </t-col>
      </t-row>
    </t-card>

    <!-- 列表 -->
    <t-card :bordered="false" class="table-card">
      <div class="toolbar">
        <div class="toolbar__title">
          <t-icon name="history" class="toolbar__icon" />
          <span>变更日志</span>
          <t-tooltip content="库内永不存明文：脱敏在写入时完成（L1+L3），列表看到的就是脱敏值">
            <t-tag size="small" variant="light" theme="warning">脱敏存储</t-tag>
          </t-tooltip>
        </div>
        <t-space size="small">
          <span class="toolbar__count">共 {{ pagination.total }} 条</span>
          <t-button size="small" variant="text" :loading="loading" @click="loadList">
            <template #icon><t-icon name="refresh" /></template>
          </t-button>
        </t-space>
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
        <template #created_at="{ row }">
          <div class="cell-time">{{ formatDateTime(row.created_at) }}</div>
          <div class="cell-time__id">{{ row.id.slice(0, 8) }}…</div>
        </template>
        <template #resource="{ row }">
          <div class="cell-res">{{ row.resource }}</div>
          <div class="cell-res__pk" :title="row.resource_id">{{ row.resource_id }}</div>
        </template>
        <template #action="{ row }">
          <t-tag size="small" variant="light" :theme="actionTheme(row.action)">{{ row.action_label || row.action }}</t-tag>
        </template>
        <template #operator="{ row }">
          <span>{{ row.operator_name || '（系统）' }}</span>
        </template>
        <template #changed_fields="{ row }">
          <t-space size="small" break-line>
            <t-tag v-for="field in row.changed_fields.slice(0, 3)" :key="field" size="small" variant="outline">
              {{ field }}
            </t-tag>
            <t-tag v-if="row.changed_fields.length > 3" size="small" variant="outline">
              +{{ row.changed_fields.length - 3 }}
            </t-tag>
            <span v-if="row.changed_fields.length === 0" class="cell-empty">—</span>
          </t-space>
        </template>
        <template #masked="{ row }">
          <t-tag v-if="row.masked" size="small" variant="light" theme="warning">含脱敏</t-tag>
          <span v-else class="cell-empty">—</span>
        </template>
        <template #op="{ row }">
          <t-link v-privilege="[PERM.changelogList]" theme="primary" @click="openDetail(row)">详情 / diff</t-link>
        </template>
        <template #empty>
          <div class="empty-state">
            <t-icon class="empty-state__icon" name="history" />
            <p class="empty-state__title">没有符合条件的变更</p>
            <p class="empty-state__desc">只有「成功提交」的业务变更才会落这里（失败尝试不进本表）</p>
          </div>
        </template>
      </t-table>
    </t-card>
  </div>
</template>

<script setup lang="ts">
/**
 * 变更日志列表（`system:change-log:list`，导出另需 `system:change-log:export`）。
 *
 * # 口径（与后端 `changelog/handler.rs` 一致）
 *
 * - 筛选字段全部走 `ChangeLogPageQuery`，时间用 RFC3339 传（页面用 dayjs 转换）；
 * - **`keyword` 不搜表名**（后端只搜动作文案 / 操作人 / 主键，实测 `keyword=sys_help_doc`
 *   返回 0 条），要按表筛请用「资源表」下拉（映射到 `resource`）；
 * - 导出返回 **xlsx 字节流**，必须带鉴权头取 blob 再落盘（`<a href>` 会 401）；
 * - 列表里的 `masked` 是「这条 diff 含脱敏值」的标记；值本身入库时已脱敏，前端不再二次打码。
 */
import { onMounted, reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import { useRouter } from 'vue-router';
import dayjs from 'dayjs';
import { ApiError } from '@/api/request';
import { PERM } from '@/constants/permissions';
import {
  exportChangeLogs,
  getTableRegistry,
  pageChangeLogs,
  type ChangeLogListItem,
  type ChangeLogPageQuery,
} from '@/api/business/changelog-api';
import { formatDateTime } from '@/utils/format';
import { saveBlob } from '@/utils/download';

const router = useRouter();

const rows = ref<ChangeLogListItem[]>([]);
const loading = ref(false);
const exporting = ref(false);
const timeRange = ref<string[]>([]);
const query = reactive<ChangeLogPageQuery>({
  module: '',
  resource: '',
  action: '',
  operator_name: '',
  keyword: '',
});
const pagination = reactive({ current: 1, pageSize: 20, total: 0, showJumper: true, pageSizeOptions: [10, 20, 50, 100] });

/** 动作码（与后端 `Action::ALL` 逐字一致；写错就是查不到而不是报错） */
const actionOptions = [
  { label: '新增', value: 'CREATE' },
  { label: '修改', value: 'UPDATE' },
  { label: '删除', value: 'DELETE' },
  { label: '状态变更', value: 'STATUS' },
  { label: '分配', value: 'ASSIGN' },
  { label: '发布', value: 'PUBLISH' },
  { label: '下线', value: 'OFFLINE' },
  { label: '上传', value: 'UPLOAD' },
  { label: '下载', value: 'DOWNLOAD' },
  { label: '清理', value: 'CLEAN' },
  { label: '登录', value: 'LOGIN' },
];

const moduleOptions = [
  { label: 'system', value: 'system' },
  { label: 'business', value: 'business' },
];

/** 资源表选项：来自后端登记表清单（不查库，编译期常量），避免手写表名 */
const tableOptions = ref<{ label: string; value: string }[]>([]);

const columns = [
  { colKey: 'created_at', title: '发生时间', width: 160 },
  { colKey: 'resource', title: '资源 / 主键', width: 220 },
  { colKey: 'action', title: '动作', width: 110 },
  { colKey: 'operator', title: '操作人', width: 110 },
  { colKey: 'trace_source', title: '来源', width: 80 },
  { colKey: 'changed_fields', title: '变化字段', width: 240 },
  { colKey: 'masked', title: '脱敏', width: 90 },
  { colKey: 'op', title: '操作', width: 110, fixed: 'right' as const },
];

/** 动作 → 语义色 */
function actionTheme(action: string): 'primary' | 'success' | 'warning' | 'danger' | 'default' {
  switch ((action || '').toUpperCase()) {
    case 'CREATE':
      return 'success';
    case 'DELETE':
      return 'danger';
    case 'PUBLISH':
    case 'OFFLINE':
      return 'warning';
    case 'UPDATE':
      return 'primary';
    default:
      return 'default';
  }
}

/** 时间窗 → RFC3339（含首尾整天） */
function buildTimeFilter(): Pick<ChangeLogPageQuery, 'start_time' | 'end_time'> {
  const [start, end] = timeRange.value ?? [];
  return {
    start_time: start ? dayjs(start).startOf('day').toISOString() : undefined,
    end_time: end ? dayjs(end).endOf('day').toISOString() : undefined,
  };
}

async function loadList(): Promise<void> {
  loading.value = true;
  try {
    const data = await pageChangeLogs({
      ...query,
      ...buildTimeFilter(),
      page: pagination.current,
      page_size: pagination.pageSize,
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
  query.module = '';
  query.resource = '';
  query.action = '';
  query.operator_name = '';
  query.keyword = '';
  timeRange.value = [];
  handleSearch();
}

function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  pagination.current = pageInfo.current;
  pagination.pageSize = pageInfo.pageSize;
  void loadList();
}

/** 详情：带 `created_at` 过去，后端只扫一个分区 */
function openDetail(row: ChangeLogListItem): void {
  void router.push({
    path: `/system/change-log/detail/${row.id}`,
    query: { created_at: row.created_at },
  });
}

/** 导出（xlsx 字节流 + 鉴权头） */
async function handleExport(): Promise<void> {
  exporting.value = true;
  try {
    const blob = await exportChangeLogs({ ...query, ...buildTimeFilter() });
    const stamp = dayjs().format('YYYYMMDD-HHmmss');
    saveBlob(blob, `变更日志-${stamp}.xlsx`);
    void MessagePlugin.success('导出完成（脱敏数据，已记审计）');
  } catch (error) {
    showError(error);
  } finally {
    exporting.value = false;
  }
}

function showError(error: unknown): void {
  void MessagePlugin.error(error instanceof ApiError ? error.message : '请求失败，请稍后重试');
}

onMounted(async () => {
  void loadList();
  try {
    const registry = await getTableRegistry();
    tableOptions.value = registry.tables.map((entry) => ({
      label: `${entry.table}（${entry.module}）`,
      value: entry.table,
    }));
  } catch {
    // 登记表清单拉不到只影响下拉提示，不影响检索
  }
});
</script>

<style scoped lang="less">
.cl-list-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.filter-card {
  flex: 0 0 auto;
}

.toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 12px;

  &__title {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 16px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__icon {
    color: var(--td-brand-color);
  }

  &__count {
    font-size: 13px;
    color: var(--td-text-color-placeholder);
  }
}

.cell-time {
  font-size: 13px;
  color: var(--td-text-color-primary);

  &__id {
    margin-top: 2px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.cell-res {
  font-size: 13px;
  color: var(--td-text-color-primary);

  &__pk {
    margin-top: 2px;
    max-width: 200px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.cell-empty {
  color: var(--td-text-color-placeholder);
}

.empty-state {
  padding: 60px 0;
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
    font-size: 13px;
    color: var(--td-text-color-secondary);
  }
}
</style>

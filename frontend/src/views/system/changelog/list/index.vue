<!--
  变更日志列表（P3-13）。

  # 导出留在页面自己手里（不走 ListPage 的 `exportable`）
  两个理由：① 导出要 `system:change-log:export` 这道权限门，ListPage 的导出按钮没有权限入参；
  ② 导的是**服务端生成的脱敏 xlsx**，不是 ListPage 那套「当前页 CSV」。所以按钮挂 `#toolbar-end`
  （导出区之后），文案与权限码都保持原样。

  # 时间窗用 `custom` 筛选自绘
  `t-date-range-picker` 给的是 `[start, end]` 日期串，入参要转成「含首尾整天」的 RFC3339，
  这条口径留在页面的 `buildTimeFilter` 里（与改造前逐字一致）。
-->
<template>
  <div class="cl-list-page">
    <ListPage
      ref="listRef"
      title="变更日志"
      testid="changelog"
      table-testid="changelog-table"
      meta-testid="changelog-meta"
      btn-prefix="btn-changelog"
      :page-size="CHANGELOG_PAGE_SIZE"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
    >
      <template #filter-time_range="{ value, set }">
        <t-date-range-picker
          :value="rangeValueOf(value)"
          placeholder="发生时间范围"
          clearable
          allow-input
          data-testid="changelog-filter-time-range"
          @change="(next: unknown) => set(normalizeRange(next))"
        />
      </template>

      <template #head-extra>
        <t-tooltip content="库内永不存明文：脱敏在写入时完成（L1+L3），列表看到的就是脱敏值">
          <t-tag size="small" variant="light" theme="warning" data-testid="changelog-masked-badge">
            脱敏存储
          </t-tag>
        </t-tooltip>
      </template>

      <template #toolbar-end>
        <t-button
          v-privilege="[PERM.changelogExport]"
          variant="outline"
          :loading="exporting"
          data-testid="btn-changelog-export"
          @click="handleExport"
        >
          <template #icon><t-icon name="download" /></template>
          导出（脱敏 xlsx）
        </t-button>
      </template>

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
        <t-link v-privilege="[PERM.changelogList]" theme="primary" data-testid="btn-changelog-detail" @click.stop="openDetail(row)">
          详情 / diff
        </t-link>
      </template>

      <template #empty>
        <div class="empty-state" data-testid="changelog-empty">
          <t-icon class="empty-state__icon" name="history" />
          <p class="empty-state__title">没有符合条件的变更</p>
          <p class="empty-state__desc">只有「成功提交」的业务变更才会落这里（失败尝试不进本表）</p>
        </div>
      </template>
    </ListPage>
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
import { computed, onMounted, reactive, ref } from 'vue';
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
import ListPage from '@/components/ListPage/index.vue';
import type {
  ColumnDef,
  FilterDef,
  ListFetcher,
  ListQuery,
  ListQueryBuilder,
} from '@/components/ListPage/types';

/** 每页条数（改造前是 20，保持手感一致） */
const CHANGELOG_PAGE_SIZE = 20;

const router = useRouter();
const exporting = ref(false);

/** 筛选状态（`time_range` 走 `#filter-time_range` 自绘，其余是声明式控件） */
interface ChangelogFilterState {
  /** 时间区间 `[start, end]` */
  time_range: string[];
  /** 模块 */
  module: string;
  /** 资源表 */
  resource: string;
  /** 动作 */
  action: string;
  /** 操作人 */
  operator_name: string;
  /** 全局关键字 */
  keyword: string;
}

const filter = reactive<ChangelogFilterState>({
  time_range: [],
  module: '',
  resource: '',
  action: '',
  operator_name: '',
  keyword: '',
});

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

const columns: ColumnDef[] = [
  { colKey: 'created_at', title: '发生时间', width: 160 },
  { colKey: 'resource', title: '资源 / 主键', width: 220 },
  { colKey: 'action', title: '动作', width: 110 },
  { colKey: 'operator', title: '操作人', width: 110 },
  { colKey: 'trace_source', title: '来源', width: 80 },
  { colKey: 'changed_fields', title: '变化字段', width: 240 },
  { colKey: 'masked', title: '脱敏', width: 90 },
  { colKey: 'op', title: '操作', width: 110, fixed: 'right', exportable: false },
];

/** 筛选项：时间窗（自绘）+ 四个下拉 + 两个关键词 */
const filters = computed<FilterDef[]>(() => [
  { key: 'time_range', type: 'custom', defaultValue: [], testid: 'changelog-filter-time-range' },
  { key: 'module', type: 'select', placeholder: '模块', width: 130, options: moduleOptions, testid: 'changelog-filter-module' },
  {
    key: 'resource',
    type: 'select',
    placeholder: '资源表',
    width: 220,
    options: tableOptions.value,
    filterable: true,
    testid: 'changelog-filter-resource',
  },
  { key: 'action', type: 'select', placeholder: '动作', width: 130, options: actionOptions, testid: 'changelog-filter-action' },
  { key: 'operator_name', type: 'keyword', placeholder: '操作人', width: 160, testid: 'changelog-filter-operator' },
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '关键字（动作 / 操作人 / 主键）',
    width: 260,
    testid: 'changelog-filter-keyword',
  },
]);

/** 拉一页 */
const fetcher: ListFetcher<ChangeLogListItem> = (query) =>
  pageChangeLogs(query as ChangeLogPageQuery);

/** 时间窗 → RFC3339（含首尾整天） */
function buildTimeFilter(range: unknown): Pick<ChangeLogPageQuery, 'start_time' | 'end_time'> {
  const [start, end] = normalizeRange(range);
  return {
    start_time: start ? dayjs(start).startOf('day').toISOString() : undefined,
    end_time: end ? dayjs(end).endOf('day').toISOString() : undefined,
  };
}

/**
 * 入参拼装。
 *
 * 键名与改造前逐字一致；`api/business/changelog-api.ts::asParams` 会丢掉空串，
 * 所以这里原样带上筛选项即可（`module=''` 不会被发成 `?module=`）。
 */
const queryBuilder: ListQueryBuilder = (state, page, pageSize): ListQuery => {
  const { time_range: range, ...rest } = state as ChangelogFilterState;
  return {
    ...rest,
    ...buildTimeFilter(range),
    page,
    page_size: pageSize,
  };
};

/** 区间值规整成 `[start, end]` */
function normalizeRange(value: unknown): string[] {
  if (!Array.isArray(value)) {
    return [];
  }
  return value.filter((item) => item !== null && item !== undefined && item !== '').map(String);
}

/** 日期区间控件的受控值 */
function rangeValueOf(value: unknown): string[] {
  return normalizeRange(value);
}

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
    const { time_range: range, ...rest } = filter;
    const blob = await exportChangeLogs({ ...rest, ...buildTimeFilter(range) });
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

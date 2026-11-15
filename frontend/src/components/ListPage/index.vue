<!--
  通用列表页（P3-04）：筛选条 + 服务端分页表格 + 批量操作 + 行操作 + 导出。

  # 为什么要有它

  54 个 ERP 页面各写一套「筛选 → 拉页 → 分页 → 批量 → 导出」，同一处口径改八遍必然漂移
  （典型：空串筛选 `?keyword=` 查出 0 行、删到最后一页停在空页、批量完不清勾选）。
  判据全部收在 `list-page-pure.ts`（有单测），本文件只负责画和串请求。

  # 页面只声明五样东西

  `title` / `columns` / `fetcher` / `filters` / `actions`（`exportable` 可选）——
  抽屉、弹窗、附件面板、导入按钮都走插槽，组件不猜业务。

  # 插槽

  | 插槽 | 用途 |
  |------|------|
  | `#toolbar` | 筛选行右侧（spacer 之后、导出按钮之前）：导入 / 新建 |
  | `#toolbar-end` | 导出按钮之后 |
  | `#filters-extra` | 声明式筛选项之后（日期区间之类控件自绘时用） |
  | `#filter-<key>` | `FilterDef.type = 'custom'` 的筛选控件（拿到 `value` / `set`） |
  | `#cell-<colKey>`（= `#<colKey>`） | 单元格自绘，名字必须与列 `colKey` 一致 |
  | `#empty` | 整块空态（默认按「有没有筛选」给两种文案） |
  | `#head-extra` | 「共 N 条」右边的补充（如「已选 M 条」） |

  # 行操作与权限

  `actions.row[].permission` 无权限时按 `whenDenied` 处理：默认 `hide`（与 `v-privilege` 的
  `display:none` 同语义），`disable` 则置灰 + tooltip 说明缺哪个码。

  # 导出

  默认导**当前页 CSV**（带 BOM，Excel 直接双击不乱码）——服务端分页下前端手里没有全量，
  假装有就会导出半份。要全量 xlsx 由页面传 `exportTask`（后端出文件）。

  # 已知不做的

  勾选跨页保持（`utils/batch-action` 明确要求批量完清勾选）、列表记忆（`useListMemory`，
  各页取舍不同，接入是 P3-05 的事）、排序（当前 ERP 列表都没开）。
-->
<template>
  <div class="lp" :data-testid="testid">
    <!-- 筛选行 -->
    <t-card :bordered="false" class="lp__filter-card">
      <div class="lp__filter-row">
        <template v-for="def in filterDefs" :key="def.key">
          <t-input
            v-if="def.type === 'keyword'"
            v-model="filter[def.key]"
            class="lp__control"
            :style="{ width: widthOf(def) }"
            :placeholder="def.placeholder ?? '输入关键词搜索'"
            :data-testid="def.testid"
            clearable
            @enter="handleSearch"
          >
            <template #prefix-icon><t-icon name="search" /></template>
          </t-input>

          <t-select
            v-else-if="def.type === 'select'"
            v-model="filter[def.key]"
            class="lp__control"
            :style="{ width: widthOf(def) }"
            :options="def.options ?? []"
            :placeholder="def.placeholder ?? '全部'"
            :data-testid="def.testid"
            clearable
          />

          <t-input-number
            v-else-if="def.type === 'number'"
            v-model="filter[def.key]"
            class="lp__control"
            :style="{ width: widthOf(def) }"
            :placeholder="def.placeholder"
            :data-testid="def.testid"
            theme="normal"
          />

          <!-- custom / date-range：页面自绘（拿到当前值与写回口） -->
          <slot
            v-else
            :name="`filter-${def.key}`"
            :value="filter[def.key]"
            :set="setterFor(def.key)"
            :def="def"
          />
        </template>

        <slot name="filters-extra" :filter="filter" />

        <t-button
          theme="primary"
          :data-testid="`${btnPrefix}-search`"
          :loading="loading"
          @click="handleSearch"
        >
          查询
        </t-button>
        <t-button variant="outline" :data-testid="`${btnPrefix}-reset`" @click="handleReset">重置</t-button>
        <t-button variant="outline" :data-testid="`${btnPrefix}-refresh`" @click="reload">刷新</t-button>

        <span class="lp__spacer" />

        <slot name="toolbar" :reload="reload" :filter="filter" :total="total" :loading="loading" />

        <t-tooltip
          v-if="exportable"
          :content="exportTip"
          :disabled="!exportTip"
          placement="top-right"
        >
          <span class="lp__gate">
            <t-button
              variant="outline"
              :data-testid="`${btnPrefix}-export`"
              :disabled="exporting || rows.length === 0"
              @click="handleExport"
            >
              <template #icon><t-icon name="download" /></template>
              导出
            </t-button>
          </span>
        </t-tooltip>

        <slot name="toolbar-end" :reload="reload" />
      </div>
    </t-card>

    <!-- 列表 -->
    <t-card :bordered="false" class="lp__list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">{{ title }}</span>
          <span class="panel-head__meta" :data-testid="metaTestid">共 {{ total }} 条</span>
        </div>
        <slot name="head-extra" :rows="rows" :total="total" />
      </div>

      <t-alert v-if="loadError" theme="error" :message="loadError" class="lp__alert">
        <template #operation>
          <t-button size="small" variant="outline" @click="reload">重试</t-button>
        </template>
      </t-alert>

      <BatchActionBar
        v-if="batchReady"
        :selected="selectedRows"
        :actions="batchActions"
        :runner="batchRunner"
        :filtered-total="total"
        :fetch-all-ids="fetchAllIds"
        :testid="`${testid}-batch`"
        @update:selected="onSelectedRows"
        @done="onBatchDone"
      />

      <t-table
        row-key="id"
        :data="rows"
        :columns="tableColumns"
        :loading="loading"
        :pagination="pagination"
        :hover="true"
        :selected-row-keys="batchReady ? selectedIds : undefined"
        :data-testid="tableTestid"
        @page-change="handlePageChange"
        @select-change="onSelectChange"
      >
        <!-- 单元格自绘：槽名 = 列 colKey（`#status` / `#contact_person` …），原样转给 t-table -->
        <template v-for="item in cellSlots" :key="item.source" #[item.target]="slotProps">
          <slot :name="item.source" v-bind="slotProps" />
        </template>

        <!-- 行操作：页面没自绘 op 列时按声明渲染 -->
        <template v-if="!hasCustomOpSlot" #op="{ row }">
          <div class="lp__ops">
            <template v-for="action in visibleRowActions(row)" :key="action.key">
              <t-tooltip v-if="rowActionBlockedReason(row, action)" :content="rowActionBlockedReason(row, action)">
                <span class="lp__op-disabled" :data-testid="actionTestid(action)">{{ action.label }}</span>
              </t-tooltip>
              <t-link
                v-else
                :theme="action.theme ?? 'primary'"
                :data-testid="actionTestid(action)"
                @click="runRowAction(row, action)"
              >
                {{ action.label }}
              </t-link>
            </template>
          </div>
        </template>

        <template #empty>
          <slot name="empty" :filtered="filtered" :total="total">
            <div class="lp__empty" :data-testid="`${testid}-empty`">
              <t-icon class="lp__empty-icon" name="file" />
              <p class="lp__empty-title">{{ filtered ? '没有匹配的记录' : `还没有${title}` }}</p>
              <p class="lp__empty-desc">
                {{ filtered ? '换个关键词 / 筛选条件试试，或点「重置」看全部' : '先在上面新建一条，列表就有数据了' }}
              </p>
            </div>
          </slot>
        </template>
      </t-table>
    </t-card>
  </div>
</template>

<script setup lang="ts">
/**
 * 通用列表页（P3-04 试点：`business/erp/supplier`）。
 *
 * 见文件头 HTML 注释：组件只做五件事（筛选 / 分页 / 批量 / 行操作 / 导出），
 * 判据在 `list-page-pure.ts`，业务抽屉与弹窗留插槽给页面。
 */
import { computed, reactive, ref, useSlots } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import BatchActionBar from '@/components/BatchActionBar.vue';
import { useBatchSelection, type SelectableRow } from '@/composables/useBatchSelection';
import { useUserStore } from '@/stores/user';
import { saveBlob } from '@/utils/download';
import type { BatchOutcome, BatchRow, BatchRunner } from '@/utils/batch-action';
import {
  buildListQuery,
  clampPage,
  csvFileName,
  emptyFilter,
  errorText,
  isFiltered,
  paginationOf,
  rowsToCsv,
} from './list-page-pure';
import type {
  ColumnDef,
  ExportContext,
  FilterDef,
  ListActions,
  ListFilter,
  ListFetcher,
  ListQuery,
  ListQueryBuilder,
  RowActionDef,
} from './types';

const props = withDefaults(
  defineProps<{
    /** 列表标题（也用于空态文案与导出文件名） */
    title: string;
    /** 表格列（colKey 与后端出参同名） */
    columns: ColumnDef[];
    /** 拉一页：入参由 `queryBuilder` 决定，出参是后端 `PageData` */
    fetcher: ListFetcher<any>;
    /** 声明式筛选（`keyword` / `select` / `number` / `custom`） */
    filters?: FilterDef[];
    /** 自定义入参拼装（页面已有被单测钉住的 `buildXxxQuery` 时传它） */
    queryBuilder?: ListQueryBuilder;
    /** 批量 / 行内操作 */
    actions?: ListActions;
    /** 批量执行器（声明 `actions.batch` 时必须给） */
    batchRunner?: BatchRunner;
    /** 「选中全部筛选结果」时怎么把全部行拿回来；不传就不显示该链接 */
    fetchAllIds?: () => Promise<BatchRow[]>;
    /** 是否显示导出按钮 */
    exportable?: boolean;
    /** 服务端导出（不传则导当前页 CSV） */
    exportTask?: (ctx: ExportContext) => Promise<void>;
    /** 每页条数 */
    pageSize?: number;
    /** 分页器是否允许改每页条数 */
    showPageSize?: boolean;
    /** 组件根 testid（默认 `listpage`） */
    testid?: string;
    /** 表格 testid（默认 `${testid}-table`） */
    tableTestid?: string;
    /** 「共 N 条」testid（默认 `${testid}-meta`） */
    metaTestid?: string;
    /** 查询/重置/刷新/导出按钮的 testid 前缀（默认 `${testid}`） */
    btnPrefix?: string;
  }>(),
  {
    filters: () => [],
    queryBuilder: undefined,
    actions: undefined,
    batchRunner: undefined,
    fetchAllIds: undefined,
    exportable: false,
    exportTask: undefined,
    pageSize: 20,
    showPageSize: false,
    testid: 'listpage',
    tableTestid: undefined,
    metaTestid: undefined,
    btnPrefix: undefined,
  },
);

const emit = defineEmits<{
  /** 一页拉完（页面可据此联动别的区域） */
  (event: 'loaded', payload: { rows: any[]; total: number; page: number }): void;
  /** 点过「重置」 */
  (event: 'reset'): void;
  /** 行操作 / 批量动作执行完（页面可刷新关联数据） */
  (event: 'batch-done', outcome: BatchOutcome): void;
}>();

const slots = useSlots();
const userStore = useUserStore();

// ==================== 派生 testid ====================

const tableTestid = computed(() => props.tableTestid ?? `${props.testid}-table`);
const metaTestid = computed(() => props.metaTestid ?? `${props.testid}-meta`);
const btnPrefix = computed(() => props.btnPrefix ?? props.testid);

// ==================== 列表状态 ====================

const filterDefs = computed(() => props.filters ?? []);
const filter = reactive<ListFilter>(emptyFilter(props.filters));
const rows = ref<any[]>([]);
const total = ref(0);
const current = ref(1);
const loading = ref(false);
const loadError = ref('');
const exporting = ref(false);
/** 已初始化过一次（首屏拉过；避免空列表 + 未加载时把「还没有数据」先闪出来） */
const loadedOnce = ref(false);

const filtered = computed(() => isFiltered(filter, filterDefs.value));
const pagination = computed(() => paginationOf(current.value, props.pageSize, total.value, props.showPageSize));
const exportTip = computed(() =>
  props.exportable && !props.exportTask ? '导出当前页（CSV）；要全量请联系管理员走导入导出任务' : '',
);

/** 筛选控件宽度 */
function widthOf(def: FilterDef): string {
  if (def.width) {
    return `${def.width}px`;
  }
  if (def.type === 'keyword') {
    return '300px';
  }
  if (def.type === 'select') {
    return '150px';
  }
  return '140px';
}

/** 当前页入参 */
function queryOf(page: number): ListQuery {
  if (props.queryBuilder) {
    return props.queryBuilder(filter, page, props.pageSize);
  }
  return buildListQuery(filter, filterDefs.value, page, props.pageSize);
}

/**
 * 拉一页。
 *
 * 页码越界（删掉最后一页最后一条）时**先回夹再拉**，别停在空页面上让用户以为数据没了。
 */
async function load(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    let data = await props.fetcher(queryOf(page));
    let resolvedPage = data.page ?? page;
    const fixed = clampPage(resolvedPage, data.total ?? 0, props.pageSize);
    if (fixed !== resolvedPage) {
      resolvedPage = fixed;
      data = await props.fetcher(queryOf(fixed));
    }
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
    current.value = data.page ?? resolvedPage;
    loadedOnce.value = true;
    emit('loaded', { rows: rows.value, total: total.value, page: current.value });
  } catch (error) {
    rows.value = [];
    total.value = 0;
    loadError.value = `${props.title}加载失败：${errorText(error)}`;
  } finally {
    loading.value = false;
  }
}

/** 刷新当前页 */
function reload(): void {
  void load(current.value);
}

/** 查询（回第 1 页） */
function handleSearch(): void {
  clearSelection();
  current.value = 1;
  void load(1);
}

/** 重置（清筛选 + 回第 1 页） */
function handleReset(): void {
  Object.assign(filter, emptyFilter(props.filters));
  clearSelection();
  current.value = 1;
  emit('reset');
  void load(1);
}

/** 切页 */
function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  clearSelection();
  current.value = pageInfo.current;
  void load(pageInfo.current);
}

// ==================== 批量 ====================

const batchActions = computed(() => props.actions?.batch ?? []);
const batchReady = computed(() => batchActions.value.length > 0 && typeof props.batchRunner === 'function');
/** `BatchActionBar` 的 runner 必填；`batchReady` 已经判过是函数 */
const batchRunner = computed(() => props.batchRunner as BatchRunner);

/**
 * 批量勾选状态。
 *
 * 行结构只要 `id`（+ 可选 `status`）；ERP 单据的状态是文本码，主数据的 `status` 是 0/1，
 * 这里统一转成字符串（`BatchActionSpec.allowedStatus` 比对的就是字符串）。
 */
const selectableRows = computed<SelectableRow[]>(() =>
  rows.value.map((row) => ({
    id: String(row.id),
    status: row.status === undefined || row.status === null ? null : String(row.status),
  })),
);
const selection = useBatchSelection(batchActions.value, selectableRows);
const selectedIds = selection.selectedIds;
const selectedRows = selection.selectedRows;
const onSelectChange = selection.onSelectChange;
const onSelectedRows = selection.onSelectedRows;
const clearSelection = selection.clearSelection;
const isSelectable = selection.isSelectable;

/** 一批跑完：清勾选已由 `BatchActionBar` 触发，这里只刷列表 */
function onBatchDone(outcome: BatchOutcome): void {
  emit('batch-done', outcome);
  void load(current.value);
}

// ==================== 列 ====================

/** 行操作列：页面列里已有 `op` 就尊重页面的列宽，否则补一列 */
const rowActions = computed<Array<RowActionDef<any>>>(() => props.actions?.row ?? []);

const tableColumns = computed<ColumnDef[]>(() => {
  const cols = [...props.columns];
  if (batchReady.value) {
    cols.unshift({
      colKey: 'row-select',
      type: 'multiple',
      width: 50,
      fixed: 'left',
      disabled: (options: { row: any; rowIndex: number }) => !isSelectable(options.row),
    });
  }
  if (rowActions.value.length === 0) {
    return cols;
  }
  if (cols.some((column) => column.colKey === 'op')) {
    return cols;
  }
  return [...cols, { colKey: 'op', title: '操作', width: 200, fixed: 'right' }];
});

/** 页面自绘了 op 列（`#cell-op` / `#op`）时不再渲染声明式行操作 */
const hasCustomOpSlot = computed(() => Boolean(slots['cell-op'] ?? slots.op) && rowActions.value.length === 0);

/**
 * 要转给 t-table 的单元格插槽。
 *
 * 页面写 `#status`（= 列 colKey）或 `#cell-status` 都认，统一按列名 `status` 转给 t-table。
 */
const cellSlots = computed(() => {
  const colKeys = new Set(
    props.columns.map((column) => column.colKey).filter((key): key is string => typeof key === 'string' && key !== 'op'),
  );
  return Object.keys(slots)
    .map((name) => ({ source: name, target: name.replace(/^cell-/, '') }))
    .filter((item) => colKeys.has(item.target));
});

// ==================== 筛选写回 ====================

/** `custom` 筛选控件的写回口（模板里不能写 TS 箭头函数带类型注解） */
function setFilter(key: string, value: unknown): void {
  filter[key] = value;
}

/** 给插槽用的写回函数（绑定成 `:set`，避免模板里写内联箭头函数） */
function setterFor(key: string): (value: unknown) => void {
  return (value: unknown) => setFilter(key, value);
}

// ==================== 行操作 ====================

/** 该行要显示的操作（无权限且 `hide` 的整条不渲染） */
function visibleRowActions(_row: any): Array<RowActionDef<any>> {
  return rowActions.value.filter((action) => {
    if (!action.permission) {
      return true;
    }
    if (userStore.hasPermission(action.permission)) {
      return true;
    }
    return action.whenDenied === 'disable';
  });
}

/** 置灰原因（空 = 可点） */
function rowActionBlockedReason(row: any, action: RowActionDef<any>): string | undefined {
  if (action.permission && !userStore.hasPermission(action.permission)) {
    return `当前账号没有 ${action.permission} 权限`;
  }
  if (action.disabled?.(row)) {
    return action.tip?.(row) ?? `${action.label}：当前行不支持该操作`;
  }
  return undefined;
}

function actionTestid(action: RowActionDef<any>): string {
  return action.testid ?? `${props.testid}-${action.key}`;
}

async function runRowAction(row: any, action: RowActionDef<any>): Promise<void> {
  try {
    await action.handler(row);
  } catch (error) {
    MessagePlugin.error(`${action.label}失败：${errorText(error)}`);
  }
}

// ==================== 导出 ====================

/** 导出：给了 `exportTask` 走服务端，否则导当前页 CSV */
async function handleExport(): Promise<void> {
  if (rows.value.length === 0) {
    MessagePlugin.warning('当前页没有数据可导出');
    return;
  }
  exporting.value = true;
  try {
    if (props.exportTask) {
      await props.exportTask({
        filter,
        query: queryOf(current.value),
        rows: rows.value,
        columns: props.columns,
        total: total.value,
      });
    } else {
      const csv = rowsToCsv(rows.value, props.columns);
      saveBlob(new Blob([csv], { type: 'text/csv;charset=utf-8' }), csvFileName(props.title));
      MessagePlugin.success(`已导出当前页 ${rows.value.length} 行（CSV）`);
    }
  } catch (error) {
    MessagePlugin.error(`导出失败：${errorText(error)}`);
  } finally {
    exporting.value = false;
  }
}

defineExpose({
  /** 重新拉当前页 */
  reload,
  /** 查询（回第 1 页） */
  search: handleSearch,
  /** 重置筛选 */
  reset: handleReset,
  /** 当前页数据 */
  rows,
  /** 总条数 */
  total,
  /** 当前页码 */
  current,
  /** 筛选状态 */
  filter,
  /** 是否已拉过首屏 */
  loadedOnce,
  /** 清空批量勾选 */
  clearSelection,
  /** 按声明重新拉一页（页面在新建/删除后调用） */
  load,
});

// 首屏
void load(1);
</script>

<style scoped lang="less">
.lp {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.lp__filter-card {
  flex: 0 0 auto;
}

.lp__filter-row {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.lp__spacer {
  flex: 1 1 auto;
}

.lp__gate {
  display: inline-flex;
}

.lp__list-card {
  flex: 1 1 auto;
  min-width: 0;
}

.panel-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 10px;
}

.panel-head__main {
  display: flex;
  align-items: baseline;
  gap: 10px;
  min-width: 0;
}

.panel-head__title {
  font-size: 14px;
  font-weight: 600;
  color: var(--td-text-color-primary);
  white-space: nowrap;
}

.panel-head__meta {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
  white-space: nowrap;
}

.lp__alert {
  margin-bottom: 8px;
}

.lp__ops {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.lp__op-disabled {
  color: var(--td-text-color-disabled);
  cursor: not-allowed;
}

.lp__empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 6px;
  padding: 48px 16px;
  text-align: center;
}

.lp__empty-icon {
  font-size: 32px;
  color: var(--td-text-color-placeholder);
}

.lp__empty-title {
  margin: 0;
  font-size: 14px;
  color: var(--td-text-color-secondary);
}

.lp__empty-desc {
  margin: 0;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
  max-width: 460px;
  line-height: 1.6;
}
</style>

<!--
  通用列表页（P3-04）：筛选条 + 服务端分页表格 + 批量操作 + 行操作 + 导出。

  # 为什么要有它

  54 个 ERP 页面各写一套「筛选 → 拉页 → 分页 → 批量 → 导出」，同一处口径改八遍必然漂移
  （典型：空串筛选 `?keyword=` 查出 0 行、删到最后一页停在空页、批量完不清勾选）。
  判据全部收在 `list-page-pure.ts`（有单测），本文件只负责画和串请求。

  # 页面只声明五样东西

  `title` / `columns` / `fetcher` / `filters` / `actions`（`exportable` 可选），
  另加三个「布局/口径」小开关：`rowKey`（行主键字段，总账流水用「凭证+行号」）、
  `metaUnit`（「共 N **条/张/个**」的量词）、`rowClassName`（给行上色，如会计期间标「当月」）。
  抽屉、弹窗、附件面板、导入按钮都走插槽，组件不猜业务。
  再有两个「行里还有一层」的小开关（只给需要展开行的列表）：`expandable`（显示展开图标）
  + `tableSlots`（额外透传的 t-table 插槽名，目前就是 `['expandedRow']`）。

  # 插槽

  | 插槽 | 用途 |
  |------|------|
  | `#toolbar` | 筛选行右侧（spacer 之后、导出按钮之前）：导入 / 新建 |
  | `#toolbar-end` | 导出按钮之后 |
  | `#filters-extra` | 声明式筛选项之后（日期区间之类控件自绘时用） |
  | `#filter-<key>` | `FilterDef.type = 'custom'` 的筛选控件（拿到 `value` / `set`） |
  | `#filter-footer` | 筛选卡内、按钮行下方（整宽）：列表记忆提示条 / 页面级信息条 |
  | `#cell-<colKey>`（= `#<colKey>`） | 单元格自绘，名字必须与列 `colKey` 一致 |
  | `#op` | 行操作列自绘（列里要有 `colKey: 'op'` 或别名的操作列，且**不要**同时声明 `actions.row`） |
  | `#empty` | 整块空态（默认按「有没有筛选」给两种文案） |
  | `#head-extra` | 「共 N 条」右边的补充（页头提示 / 「已选 M 条」） |
  | `#meta-extra` | 写进「共 N 条」**同一个元素里**的后缀（`（全部仓库）` 这类限定语；e2e 按 `*-meta` 断言文案时不能挪到旁边） |
  | `#footer` | 表格下方（页脚合计 / 说明条），仍在列表卡内 |

  # 事件

  | 事件 | 时机 |
  |------|------|
  | `loaded` | 每次拉完一页（页面据此镜像行 / 同步页码给列表记忆） |
  | `reset` | 点过「重置」 |
  | `refresh` | 点过「刷新」（页面可顺带重拉主数据字典） |
  | `selection-change` | 勾选变化 / 换页清空（`selectable` 或批量模式下） |
  | `batch-done` | 一批批量动作跑完（列表已自动刷新） |
  | `row-click` | 点了表格某一行（`{ row }`；行内的操作链接自带 `@click.stop`，不会误触） |


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
            :filterable="def.filterable === true"
            :data-testid="def.testid"
            clearable
            @change="def.searchOnChange ? handleSearch() : undefined"
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

        <!--
          查询 / 重置 / 刷新 三只按钮包成一个 flex item：
          筛选条件多（销售/财务页 6 个起）时这一行会换行，不包起来就会出现
          「查询在上一行、重置刷新在下一行」——按钮被拆散比换行本身更难用。
        -->
        <span class="lp__actions">
          <t-button
            theme="primary"
            :data-testid="`${btnPrefix}-search`"
            :loading="loading"
            @click="handleSearch"
          >
            查询
          </t-button>
          <t-button variant="outline" :data-testid="`${btnPrefix}-reset`" @click="handleReset">重置</t-button>
          <t-button variant="outline" :data-testid="`${btnPrefix}-refresh`" @click="handleRefresh">刷新</t-button>
        </span>

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

      <!-- 筛选行下方：列表记忆提示条 / 页面级提示（整宽，不属于按钮行） -->
      <slot name="filter-footer" :filter="filter" :reload="reload" />
    </t-card>

    <!-- 列表 -->
    <t-card :bordered="false" class="lp__list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">{{ title }}</span>
          <span class="panel-head__meta" :data-testid="metaTestid">
            共 {{ total }} {{ metaUnit }}<slot name="meta-extra" :rows="rows" :total="total" />
          </span>
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
        :can="batchCanFn"
        :filtered-total="total"
        :fetch-all-ids="fetchAllIds"
        :testid="`${testid}-batch`"
        @update:selected="onSelectedRows"
        @done="onBatchDone"
      />

      <t-table
        :row-key="rowKey"
        :data="rows"
        :columns="tableColumns"
        :loading="loading"
        :pagination="pagination"
        :hover="true"
        :row-class-name="rowClassName"
        :expand-icon="expandable"
        :selected-row-keys="selectionReady ? selectedIds : undefined"
        :data-testid="tableTestid"
        @page-change="handlePageChange"
        @select-change="onSelectChange"
        @row-click="onRowClick"
      >
        <!-- 单元格自绘：槽名 = 列 colKey（`#status` / `#contact_person` …），原样转给 t-table -->
        <template v-for="item in cellSlots" :key="item.source" #[item.target]="slotProps">
          <slot :name="item.source" v-bind="slotProps" />
        </template>

        <!-- 额外透传插槽（`tableSlots` 声明，如展开行 `expandedRow`）：页面自己的模板，组件不解释 -->
        <template v-for="name in extraTableSlots" :key="`extra-${name}`" #[name]="slotProps">
          <slot :name="name" v-bind="slotProps" />
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

      <!-- 表格下方：页脚合计 / 说明条（仍在列表卡内，宽度与表格对齐） -->
      <slot name="footer" :rows="rows" :total="total" />
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
import type { BatchOutcome, BatchRow, BatchRunner, BatchActionSpec } from '@/utils/batch-action';
import {
  buildListQuery,
  clampPage,
  clearFilter,
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
    /**
     * 外部筛选对象（可选）。
     *
     * 页面自己已有 `reactive` 的筛选状态（被 `buildXxxQuery` / `useListFilterMemory` /
     * `fetchAllIds` 直接引用）时传进来，组件就地读写**同一个对象** —— 这样重构只换渲染，
     * 不换「谁持有筛选状态」，那些闭包一行都不用改。
     */
    filterState?: ListFilter;
    /** 首屏页码（列表记忆恢复时用；默认 1） */
    initialPage?: number;
    /** 自定义入参拼装（页面已有被单测钉住的 `buildXxxQuery` 时传它） */
    queryBuilder?: ListQueryBuilder;
    /** 批量 / 行内操作 */
    actions?: ListActions;
    /** 批量执行器（声明 `actions.batch` 时必须给） */
    batchRunner?: BatchRunner;
    /** 批量动作的权限门（不给 = 都能点；给了就按返回值禁用 + tooltip） */
    batchCan?: (action: BatchActionSpec) => boolean;
    /**
     * 只要行勾选（不做批量动作）：如「勾几条合并成一张单」。
     *
     * 声明后组件补勾选列并 `emit('selection-change', ids)`；页面自己拿去开弹窗。
     * `actions.batch` 存在时以批量那套为准（勾选状态不重复造）。
     */
    selectable?: boolean;
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
    /**
     * 行 class（透传给 `t-table` 的 `row-class-name`）。
     *
     * 唯一用途是**跨组件还能给行上色**：会计期间页要标「当月」那一行，
     * class 由 t-table 挂到 `<tr>` 上，页面用 `:deep(.period-row--current)` 上底色。
     */
    rowClassName?: (ctx: { row: any }) => string;
    /** 组件根 testid（默认 `listpage`） */
    testid?: string;
    /** 表格 testid（默认 `${testid}-table`） */
    tableTestid?: string;
    /**
     * 是否显示展开图标（透传 `t-table` 的 `expand-icon`）。
     *
     * 只给「行里还有一层明细」的列表用（我的薪资：一行一期，展开看这一期的薪资项）；
     * 展开内容由页面在 `tableSlots: ['expandedRow']` 里自己写，组件不解释。
     */
    expandable?: boolean;
    /**
     * 额外透传给 `t-table` 的插槽名（除「槽名 = 列 colKey」的单元格插槽之外）。
     *
     * 目前唯一用途是 `['expandedRow']`：展开行模板不是某列的单元格，`cellSlots` 那条路转不过去。
     * 名字原样转发，页面写什么名字这里就声明什么名字。
     */
    tableSlots?: string[];
    /**
     * 行主键字段（默认 `id`）。
     *
     * 绝大多数 ERP 列表的主键就是 `id`；总账流水的行主键是「凭证 + 行号」
     * （同一张凭证有多行，`voucher_id` 不唯一），页面在 `fetcher` 里拼一个 `rowKey` 字段传进来，
     * 否则 t-table 会把同凭证的第二行当成重复 key。
     */
    rowKey?: string;
    /** 「共 N 条」testid（默认 `${testid}-meta`） */
    metaTestid?: string;
    /**
     * 「共 N **条**」的量词。
     *
     * 财务口径里「期间」论个、「资产卡片」论张 —— 统一成「条」读着别扭，
     * 而「共 N …」是页头唯一的计数说明，量词让页面自己定。
     */
    metaUnit?: string;
    /** 查询/重置/刷新/导出按钮的 testid 前缀（默认 `${testid}`） */
    btnPrefix?: string;
  }>(),
  {
    filters: () => [],
    filterState: undefined,
    initialPage: 1,
    queryBuilder: undefined,
    actions: undefined,
    batchRunner: undefined,
    batchCan: undefined,
    selectable: false,
    fetchAllIds: undefined,
    exportable: false,
    exportTask: undefined,
    pageSize: 20,
    showPageSize: false,
    rowClassName: undefined,
    testid: 'listpage',
    tableTestid: undefined,
    expandable: false,
    tableSlots: () => [],
    rowKey: 'id',
    metaTestid: undefined,
    metaUnit: '条',
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
  /** 行勾选变化（`selectable` 声明后；页面拿去开合并 / 对比弹窗这类非批量动作） */
  (event: 'selection-change', ids: string[]): void;
  /** 点过「刷新」（页面据此顺带刷主数据字典） */
  (event: 'refresh'): void;
  /** 点了某一行（页面拿去做「点行开详情」；行内链接自带 `@click.stop`） */
  (event: 'row-click', payload: { row: any }): void;
}>();

const slots = useSlots();
const userStore = useUserStore();

// ==================== 派生 testid ====================

const tableTestid = computed(() => props.tableTestid ?? `${props.testid}-table`);
const metaTestid = computed(() => props.metaTestid ?? `${props.testid}-meta`);
const btnPrefix = computed(() => props.btnPrefix ?? props.testid);

// ==================== 列表状态 ====================

const filterDefs = computed(() => props.filters ?? []);
/** 筛选状态：页面传了外部对象就用它（页面里的 `buildXxxQuery` 等闭包引用的是同一个对象） */
const filter: ListFilter = props.filterState ?? reactive<ListFilter>(emptyFilter(props.filters));
const rows = ref<any[]>([]);
const total = ref(0);
const current = ref(Math.max(1, Math.floor(props.initialPage ?? 1)));
/** 每页条数（分页器允许改时以用户选中的为准，初值取 props） */
const pageSize = ref(props.pageSize);
const loading = ref(false);
const loadError = ref('');
const exporting = ref(false);
/** 已初始化过一次（首屏拉过；避免空列表 + 未加载时把「还没有数据」先闪出来） */
const loadedOnce = ref(false);

const filtered = computed(() => isFiltered(filter, filterDefs.value));
const pagination = computed(() => paginationOf(current.value, pageSize.value, total.value, props.showPageSize));
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
    return props.queryBuilder(filter, page, pageSize.value);
  }
  return buildListQuery(filter, filterDefs.value, page, pageSize.value);
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
    const fixed = clampPage(resolvedPage, data.total ?? 0, pageSize.value);
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

/**
 * 「刷新」按钮：刷列表 + 抛事件让页面顺带刷主数据（供应商 / 仓库字典可能被别人改过）。
 *
 * 不给 `#refresh` 监听的页面只刷列表，行为与直接调 `reload` 一致。
 */
function handleRefresh(): void {
  emit('refresh');
  reload();
}

/** 查询（回第 1 页） */
function handleSearch(): void {
  resetSelection();
  current.value = 1;
  void load(1);
}

/** 重置（清筛选 + 回第 1 页） */
function handleReset(): void {
  clearFilter(filterDefs.value, filter);
  resetSelection();
  current.value = 1;
  emit('reset');
  void load(1);
}

/** 切页 / 改每页条数 */
function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  resetSelection();
  // 改了每页条数就回第 1 页：停在第 7 页 + 每页 100 条通常是空页
  const resized = pageInfo.pageSize !== pageSize.value;
  pageSize.value = pageInfo.pageSize;
  current.value = resized ? 1 : pageInfo.current;
  void load(current.value);
}

// ==================== 批量 ====================

const batchActions = computed(() => props.actions?.batch ?? []);
const batchReady = computed(() => batchActions.value.length > 0 && typeof props.batchRunner === 'function');
/** `BatchActionBar` 的 runner 必填；`batchReady` 已经判过是函数 */
const batchRunner = computed(() => props.batchRunner as BatchRunner);
/** 批量按钮的权限门（页面没给就都能点，与 `BatchActionBar` 的默认一致） */
const batchCanFn = computed(() => props.batchCan ?? (() => true));

/** 勾选列要不要出现：批量动作或页面单纯要行勾选 */
const selectionReady = computed(() => batchReady.value || props.selectable);

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
const onSelectedRows = selection.onSelectedRows;
const clearSelection = selection.clearSelection;
const isSelectable = selection.isSelectable;

/** 清空勾选并把「已清空」也抛给页面（换页 / 查询 / 重置时页面镜像里的 id 要跟着清） */
function resetSelection(): void {
  clearSelection();
  emit('selection-change', []);
}

/** 表格勾选变化：同步本地勾选，并把 id 列表抛给页面（合并转单这类非批量动作要用） */
function onSelectChange(keys: Array<string | number>): void {
  selection.onSelectChange(keys);
  emit('selection-change', selectedIds.value);
}

/**
 * 点行：原样抛给页面（「点行开详情」这类交互归页面）。
 *
 * 行内的操作链接由页面自己写 `@click.stop`（`#op` 插槽），不会在这里被当成行点击；
 * 声明式行操作（`actions.row`）渲染的 `t-link` 也不冒泡到 `tr` —— TDesign 的
 * `row-click` 只在单元格空白处触发（实测点链接不进这里）。
 */
function onRowClick(context: { row?: any }): void {
  if (context?.row) {
    emit('row-click', { row: context.row });
  }
}

/** 表格勾选列的禁用判据：没有批量动作时（`selectable` 模式）所有行都能勾 */
function rowDisabled(options: { row: any }): boolean {
  return batchReady.value ? !isSelectable(options.row) : false;
}

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
  if (selectionReady.value) {
    cols.unshift({
      colKey: 'row-select',
      type: 'multiple',
      width: 50,
      fixed: 'left',
      disabled: rowDisabled,
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
 * 页面写 `#status`（= 列 colKey）或 `#cell-<colKey>` 都认，统一按列名 `status` 转给 t-table。
 * `op` 列**只有页面自绘时**才转（声明式行操作由下面的内置模板画，两边同时画会出现两套操作）。
 */
const cellSlots = computed(() => {
  const declarativeOp = rowActions.value.length > 0;
  const colKeys = new Set(
    props.columns
      .map((column) => column.colKey)
      .filter((key): key is string => typeof key === 'string' && (key !== 'op' || !declarativeOp)),
  );
  return Object.keys(slots)
    .map((name) => ({ source: name, target: name.replace(/^cell-/, '') }))
    .filter((item) => colKeys.has(item.target));
});

/**
 * 额外透传插槽（`tableSlots`）。
 *
 * 只转发「页面声明了 + 页面真的写了」的名字：声明了但没写，`t-table` 会撞上一个空插槽
 * 把默认渲染也顶掉（展开行会变成一片空白）。
 */
const extraTableSlots = computed(() =>
  (props.tableSlots ?? []).filter((name) => Boolean(slots[name])),
);

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
  /** 当前筛选状态 */
  filter,
  /** 已勾选的行 id（`selectable` / 批量模式下） */
  selectedIds,
  /** 已勾选的行（批量判状态用） */
  selectedRows,
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

/* 查询 / 重置 / 刷新：换行时整体走，别把三个按钮拆到两行 */
.lp__actions {
  display: inline-flex;
  align-items: center;
  gap: 12px;
  white-space: nowrap;
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

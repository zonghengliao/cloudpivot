/**
 * ListPage 公共列表组件的契约类型（P3-04）。
 *
 * # 为什么把类型单放一个文件
 *
 * `.vue` 里的类型页面 import 不到（`import type ... from '@/components/ListPage/index.vue'`
 * 能过，但会把 SFC 的编译产物带进单测的模块图），所以契约留在 `.ts` 里，
 * 页面写 `import type { ColumnDef, RowActionDef } from '@/components/ListPage/types'`。
 *
 * # 口径：只声明「页面之间真的会漂移」的东西
 *
 * 列 / 筛选 / 行操作 / 批量动作 / 导出 —— 这五样在 54 个 ERP 页面里写法各不相同，
 * 收成声明式之后各页只剩「数据从哪来 + 点下去做什么」。抽屉、弹窗、附件面板
 * **不进**组件（每个页面长得都不一样），页面用插槽自己填。
 */
import type { PrimaryTableCol } from 'tdesign-vue-next';
import type { BatchActionSpec, BatchRunner, BatchRow } from '@/utils/batch-action';

/** 分页入参的宽松结构：真正的字段由页面自己的 `queryBuilder` 决定。 */
export type ListQuery = Record<string, any>;

/** 筛选状态（键 = `FilterDef.key`）。 */
export type ListFilter = Record<string, any>;

/** 后端分页出参（与 `api/request.ts::PageData` 同构；不直接 import 是为了让本文件能被单测加载）。 */
export interface ListPageData<T> {
  list: T[];
  total: number;
  page: number;
  page_size: number;
}

/** 拉一页数据的函数（卡面 `fetcher`）。 */
export type ListFetcher<T> = (query: ListQuery) => Promise<ListPageData<T>>;

/**
 * 自定义入参拼装（可选）。
 *
 * 默认拼装见 {@link buildListQuery}：trim、空值整键缺席。页面已有自己那套**被单测钉住**的
 * 拼装函数（如 `buildSupplierQuery`）时传它，别让重构顺手改了接口契约。
 * 形参用 `any`：各页的筛选结构本来就不同（`SupplierFilterState` 之类），
 * 收窄成 `Record<string, any>` 反而要求页面把自己的强类型往下兼容。
 */
export type ListQueryBuilder = (filter: any, page: number, pageSize: number) => ListQuery;

/** 筛选控件种类。`custom` 走插槽 `#filter-<key>`。 */
export type FilterControl = 'keyword' | 'select' | 'number' | 'date-range' | 'custom';

/** 一个筛选项的声明。 */
export interface FilterDef {
  /** 对应 `filter` 里的字段名，也是出参键名（`date-range` 除外的拆键见文档）。 */
  key: string;
  /** 控件种类。 */
  type: FilterControl;
  /** 占位文案 / 无标签控件的提示。 */
  placeholder?: string;
  /** `select` 的选项（值里**不要**放 `''` 当「全部」：TDesign 把空串当未选，显示成 placeholder）。 */
  options?: Array<{ label: string; value: string | number }>;
  /** `select` 是否可搜索（供应商 / 物料这类几百条的字典必须开，否则要滚半天）。 */
  filterable?: boolean;
  /** 值一变就查询（老页面上 `@change="handleSearch"` 的行为，保持手感一致）。 */
  searchOnChange?: boolean;
  /** 控件宽度（px），默认 keyword 300 / select 150 / number 140。 */
  width?: number;
  /** 空筛选时的初值，默认 `''`（`number` / `date-range` 默认不设）。 */
  defaultValue?: string | number;
  /** e2e / 单测选择器，挂到控件外层 `data-testid`。 */
  testid?: string;
}

/** 表格列：TDesign 的列定义 + 导出开关（行类型交给 `t-table` 的 `data`，这里不泛型化）。 */
export type ColumnDef = PrimaryTableCol & {
  /** `false` = 这列不进导出（操作列、附件列、纯展示按钮列）。默认进。 */
  exportable?: boolean;
};

/** 行操作的按钮主题（与 `t-link` 一致）。 */
export type RowActionTheme = 'primary' | 'danger' | 'default' | 'success' | 'warning';

/** 一行上「操作」列里的一个链接（卡面 `actions.row`）。 */
export interface RowActionDef<T = any> {
  /** 动作 key（同时用于默认 testid 后缀）。 */
  key: string;
  /** 链接文案。 */
  label: string;
  /** 链接主题，默认 `primary`。 */
  theme?: RowActionTheme;
  /** 权限码：无权限时按 {@link whenDenied} 处理（默认隐藏，与 `v-privilege` 同款）。 */
  permission?: string;
  /** 无权限时：`hide`（默认）或 `disable`（置灰 + tooltip）。 */
  whenDenied?: 'hide' | 'disable';
  /** 按行置灰。 */
  disabled?: (row: T) => boolean;
  /** 置灰原因（tooltip 文案）。 */
  tip?: (row: T) => string | undefined;
  /** e2e / 单测选择器。 */
  testid?: string;
  /** 点下去做什么（弹窗 / 调接口 / 打开抽屉都在页面自己的 handler 里）。 */
  handler: (row: T) => void | Promise<void>;
}

/** 批量动作（直接复用 D7 的声明，`BatchActionBar` 的判据全在 `utils/batch-action`）。 */
export type { BatchActionSpec, BatchRunner, BatchRow };

/** `actions` 入参。 */
export interface ListActions<T = any> {
  /** 批量动作（声明后组件自动挂 `useBatchSelection` + `BatchActionBar`）。 */
  batch?: BatchActionSpec[];
  /** 行内操作（声明后组件自动补一列「操作」）。 */
  row?: Array<RowActionDef<T>>;
}

/** 导出上下文（页面要服务端导出时用 `exportTask`）。 */
export interface ExportContext {
  /** 当前筛选值 */
  filter: ListFilter;
  /** 当前页入参（含 page / page_size） */
  query: ListQuery;
  /** 当前页的行 */
  rows: any[];
  /** 当前列定义 */
  columns: ColumnDef[];
  /** 当前总条数 */
  total: number;
}

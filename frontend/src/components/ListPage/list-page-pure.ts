/**
 * ListPage 的纯逻辑（P3-04）——不碰 DOM、不碰请求，可被 `tsx --test` 直接加载。
 *
 * 组件 `index.vue` 只负责「画 + 串请求」，判断全在这里：拼入参、判空筛选、
 * 越界页码回夹、导出的文件名与 CSV 单元格转义。**改这里要连单测一起改**，
 * `tests/unit/list-page.test.ts` 钉住了每一条口径。
 */
import type { ColumnDef, FilterDef, ListFilter, ListQuery } from './types';

/** 空值判据：`null` / `undefined` / 空串 / 全空格 / 空数组都算空。`0` 与 `false` **不算空**。 */
export function isEmptyValue(value: unknown): boolean {
  if (value === null || value === undefined) {
    return true;
  }
  if (typeof value === 'string') {
    return value.trim() === '';
  }
  if (Array.isArray(value)) {
    return value.length === 0 || value.every((item) => isEmptyValue(item));
  }
  return false;
}

/** 空筛选对象（键齐全，控件才是受控的）。 */
export function emptyFilter(defs: FilterDef[] | undefined): ListFilter {
  const filter: ListFilter = {};
  for (const def of defs ?? []) {
    if (def.defaultValue !== undefined) {
      filter[def.key] = def.defaultValue;
    } else if (def.type === 'keyword' || def.type === 'select') {
      filter[def.key] = '';
    }
  }
  return filter;
}

/**
 * 清空筛选（「重置」按钮）。
 *
 * 与 {@link emptyFilter} 的差别：这里**按声明逐键写回**，`custom`（日期区间 / 复选框）这类
 * 控件也要清 —— `emptyFilter` 只覆盖 `keyword` / `select` / `defaultValue`，用它做重置会把
 * 日期区间留在原地，用户以为「重置了还筛着」。声明外的键（页面自己的私有状态）不动。
 */
export function clearFilter(defs: FilterDef[] | undefined, filter: ListFilter): ListFilter {
  for (const def of defs ?? []) {
    if (def.defaultValue !== undefined) {
      filter[def.key] = def.defaultValue;
    } else if (def.type === 'keyword' || def.type === 'select') {
      filter[def.key] = '';
    } else {
      filter[def.key] = undefined;
    }
  }
  return filter;
}

/**
 * 拼分页入参。
 *
 * **空值一律整键缺席**：`?keyword=` 会被后端当成「等于空串」去过滤，查出 0 行
 * （供应商页 `buildSupplierQuery` 的头注释记的是同一条教训）。
 * 值为 `0` / `false` 的筛选项是**真条件**，必须发出去。
 */
export function buildListQuery(
  filter: ListFilter,
  defs: FilterDef[] | undefined,
  page: number,
  pageSize: number,
): ListQuery {
  const query: ListQuery = { page, page_size: pageSize };
  for (const def of defs ?? []) {
    const raw = filter[def.key];
    if (isEmptyValue(raw)) {
      continue;
    }
    query[def.key] = typeof raw === 'string' ? raw.trim() : raw;
  }
  return query;
}

/** 是否处于筛选态（决定空态文案是「没匹配」还是「还没数据」）。 */
export function isFiltered(filter: ListFilter, defs: FilterDef[] | undefined): boolean {
  return activeFilterCount(filter, defs) > 0;
}

/** 生效筛选项个数。 */
export function activeFilterCount(filter: ListFilter, defs: FilterDef[] | undefined): number {
  let count = 0;
  for (const def of defs ?? []) {
    if (!isEmptyValue(filter[def.key])) {
      count += 1;
    }
  }
  return count;
}

/**
 * 页码回夹。
 *
 * 删掉最后一页的最后一条后，后端会返回 `page > 总页数` 的空列表 —— 停在空页面上
 * 用户以为数据全没了，所以夹回最后一页再拉一次。
 */
export function clampPage(page: number, total: number, pageSize: number): number {
  const size = pageSize > 0 ? pageSize : 1;
  const lastPage = Math.max(1, Math.ceil((total > 0 ? total : 0) / size));
  if (!Number.isFinite(page) || page < 1) {
    return 1;
  }
  return Math.min(Math.floor(page), lastPage);
}

/** 组件里 `t-table` 的分页器入参。 */
export function paginationOf(page: number, pageSize: number, total: number, showPageSize = false): {
  current: number;
  pageSize: number;
  total: number;
  showJumper: boolean;
  showPageSize: boolean;
  pageSizeOptions: number[];
} {
  return {
    current: page,
    pageSize,
    total,
    showJumper: true,
    showPageSize,
    pageSizeOptions: [10, 20, 50, 100],
  };
}

/** 错误 → 一句话（`ApiError` / `Error` / 字符串 / 未知都吃）。 */
export function errorText(error: unknown): string {
  if (error instanceof Error) {
    return error.message || String(error);
  }
  const message = (error as { message?: unknown } | null | undefined)?.message;
  if (typeof message === 'string' && message.trim() !== '') {
    return message;
  }
  return String(error);
}

/** CSV 单元格转义：含逗号 / 引号 / 换行就加引号，内部引号翻倍。 */
export function escapeCsvCell(value: unknown): string {
  if (value === null || value === undefined) {
    return '';
  }
  let text: string;
  if (Array.isArray(value)) {
    text = value.map((item) => escapeCsvCell(item)).join(';');
  } else if (typeof value === 'object') {
    text = JSON.stringify(value);
  } else {
    text = String(value);
  }
  if (/[",\r\n]/.test(text)) {
    return `"${text.replace(/"/g, '""')}"`;
  }
  return text;
}

/** 导出用的列：`colKey` 是字符串、非 `op`、`exportable !== false`。 */
export function exportColumns(columns: ColumnDef[]): ColumnDef[] {
  return columns.filter((column) => {
    if (column.exportable === false) {
      return false;
    }
    const key = column.colKey;
    if (typeof key !== 'string' || key === '' || key === 'op' || key === 'row-select') {
      return false;
    }
    return true;
  });
}

/**
 * 当前页 → CSV 文本（带 BOM，Excel 直接双击不乱码）。
 *
 * 只导**当前页**：服务端分页下前端手里没有全量数据，假装有就会导出半份。
 * 要全量导出走页面自己的 `exportTask`（后端出 xlsx）。
 */
export function rowsToCsv(rows: any[], columns: ColumnDef[]): string {
  const picked = exportColumns(columns);
  const header = picked.map((column) => escapeCsvCell(String(column.title ?? column.colKey))).join(',');
  const lines = rows.map((row) =>
    picked
      .map((column) => escapeCsvCell(readCell(row, column.colKey as string)))
      .join(','),
  );
  return `\ufeff${[header, ...lines].join('\r\n')}\r\n`;
}

/** 取单元格值（`a.b` 形式的 colKey 也认；缺字段给空串）。 */
function readCell(row: any, colKey: string): unknown {
  if (row === null || row === undefined) {
    return '';
  }
  const direct = row[colKey];
  if (direct !== undefined || !colKey.includes('.')) {
    return direct;
  }
  return colKey.split('.').reduce<any>((acc, key) => (acc === null || acc === undefined ? acc : acc[key]), row);
}

/** 导出文件名：`供应商-20261115-1730.csv`（本地时间，不带秒 —— 同一分钟重名就覆盖，比乱序号好认）。 */
export function csvFileName(title: string, now: Date = new Date()): string {
  const pad = (value: number) => String(value).padStart(2, '0');
  const stamp = `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}-${pad(now.getHours())}${pad(
    now.getMinutes(),
  )}`;
  const base = (title || '列表').trim().replace(/[\\/:*?"<>|]/g, '');
  return `${base || '列表'}-${stamp}.csv`;
}

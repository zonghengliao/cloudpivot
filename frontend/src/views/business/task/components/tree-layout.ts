/**
 * 任务列表「树形表格」的纯逻辑（无 Vue 依赖，可被 `tsx --test` 直接跑）。
 *
 * 为什么要单独抽出来（T8-B）：
 * - 卡面验收要求「箭头在标题列 / 父行竖条 / 子行连接线 / 父行显示 N/M 子任务进度」，
 *   这几条都是**可以用断言说清**的事实，但它们原先埋在 `index.vue` 的模板与 `rowClassName`
 *   里，`vue-tsc` 管不到、单测碰不到；
 * - 尤其是 `treeNodeColumnIndex`：E6b 写死 `0`（任务号列），而列顺序一旦有人插一列，
 *   箭头就会静默跑到别的列去。这里改成**按 `colKey === 'title'` 反查下标**，
 *   列顺序变了也照样对准标题列。
 *
 * 口径（与卡面 T8-B 一致）：
 * - 父任务 = `child_count > 0`（有子任务的行才渲染箭头、才显示子任务进度条）；
 * - 子任务 = `_isSub`（懒加载时打的标），子行优先按子行渲染（哪怕它自己也有子任务）；
 * - 子任务进度百分比 = `round(done / total * 100)`，分母为 0 时取 0，越界值先夹紧。
 */

/** 每层缩进（px）。卡面定 24：比 E6b 的 16 更容易看出层级，又不至于第 8 层跑到屏幕外。 */
export const TREE_INDENT = 24;

/** 父任务行 class（4px 主色左竖条 + 浅底 + 加粗） */
export const ROW_PARENT = 'task-row--parent';
/** 子任务行 class（连接线 + 更浅底） */
export const ROW_CHILD = 'task-row--child';

/** 树形行至少要有的字段（`TaskRow` 是它的超集） */
export interface TreeRowLike {
  /** 子任务总数（后端 `TaskCardRes.child_count`） */
  child_count?: number;
  /** 是否子行（懒加载子任务时打的标） */
  _isSub?: boolean;
}

/** 该行是否父任务（有子任务 → 渲染箭头 + 子任务进度条） */
export function isParentRow(row: TreeRowLike | null | undefined): boolean {
  return (row?.child_count ?? 0) > 0;
}

/**
 * 行 class（父行 / 子行 / 普通行）。
 *
 * 子行**优先**：一条既有父任务又有 `_isSub` 的孙任务，视觉上仍是「树里的一枝」，
 * 不该再挂 4px 竖条（竖条只标顶层父任务）。
 */
export function rowClassNameOf(row: TreeRowLike | null | undefined): string {
  if (row?._isSub) {
    return ROW_CHILD;
  }
  return isParentRow(row) ? ROW_PARENT : '';
}

/**
 * 子任务完成度百分比（取整）。
 *
 * - `total <= 0` → 0（没有子任务就不该画进度，调用方应先判 `isParentRow`）；
 * - `done` 越界（负数 / 大于 total，脏数据或并发下架）先夹紧再算，避免出现 -50% / 150%。
 */
export function childProgressPercent(done: number | null | undefined, total: number | null | undefined): number {
  const safeTotal = typeof total === 'number' && Number.isFinite(total) ? total : 0;
  if (safeTotal <= 0) {
    return 0;
  }
  const rawDone = typeof done === 'number' && Number.isFinite(done) ? done : 0;
  const safeDone = Math.min(Math.max(rawDone, 0), safeTotal);
  return Math.round((safeDone / safeTotal) * 100);
}

/** 子任务进度文案 `N/M`（卡片「分数」部分；与进度条同源，避免百分比和分数打架） */
export function childProgressText(done: number | null | undefined, total: number | null | undefined): string {
  const safeTotal = typeof total === 'number' && Number.isFinite(total) ? Math.max(total, 0) : 0;
  const rawDone = typeof done === 'number' && Number.isFinite(done) ? done : 0;
  const safeDone = Math.min(Math.max(rawDone, 0), safeTotal);
  return `${safeDone}/${safeTotal}`;
}

/**
 * 树形箭头的列下标：反查 `colKey === 'title'`。
 *
 * 找不到 title 列（改名 / 传了空数组）时退回 0，保证表格仍然能渲染出箭头，
 * 不至于因为一次重构把树形彻底弄哑。
 */
export function titleColumnIndex(colKeys: ReadonlyArray<string | undefined>): number {
  const index = colKeys.findIndex((key) => key === 'title');
  return index >= 0 ? index : 0;
}

/** 树形表格配置（`t-enhanced-table` 的 `tree` 属性） */
export function treeConfigOf(colKeys: ReadonlyArray<string | undefined>): {
  treeNodeColumnIndex: number;
  indent: number;
} {
  return { treeNodeColumnIndex: titleColumnIndex(colKeys), indent: TREE_INDENT };
}

/**
 * 三态（loading / empty / error）的判据（P3-17）。
 *
 * # 为什么把判据抽出来
 *
 * 三态是「看起来都一样、其实每页说法不同」的东西：54 个列表页手写空态文案、
 * 40 多个页面各写一遍 `v-if="error"` + 重试按钮，同一种「加载失败」能有七八种写法。
 * 组件只负责画，**默认文案 / 兜底规则 / 上限**全收在这里，页面只声明 `state` 与差异化文案。
 *
 * # 五条定死的规则（单测逐条钉住）
 *
 * 1. **state 归一**：只认 `loading` / `empty` / `error`，其他值（含拼错的 `loading..`）
 *    一律按 `empty` 画 —— 宁可少一个骨架屏，也不要因为拼错永久转圈。
 * 2. **loading**：`variant` 只认 `skeleton` / `spinner`（非法值回骨架屏）；行数钳在 1..12，
 *    默认 4 —— 骨架屏行数超屏会顶出滚动条，比不加载更吓人。
 * 3. **error 一定可重试**：`actionText` 不给就默认「重试」；要「不可重试」（如 403 无权限）
 *    由调用方**显式传空串** `:action-text="''"`，此时整颗按钮不画。`undefined` 与 `''` 语义不同。
 * 4. **error 的插画固定 `network-error`**：空态才允许 `empty / success / fail / maintenance`
 *    （调用方传 `empty` 给 error 视为没传，避免「加载失败配一张「暂无数据」的图」）。
 * 5. **文案一律单行化**：折叠换行/连续空白、去掉首尾空格，超 240 字截断加省略号 ——
 *    后端 `msg` 带换行直接把 `t-empty` 的描述撑成多行块，观感像报错弹窗。
 */

/** 三态 */
export type StateViewState = 'loading' | 'empty' | 'error';

/** loading 的两种形态 */
export type StateViewLoadingVariant = 'skeleton' | 'spinner';

/** 空态 / 错误态可用的内置插画（t-empty 的 `type`） */
export type StateViewEmptyType = 'empty' | 'success' | 'fail' | 'network-error' | 'maintenance';

/** 默认文案：页面不给就落在这里（改文案只改这一处） */
export const STATE_VIEW_TEXT = {
  /** loading + spinner 的说明文字 */
  loading: '加载中…',
  /** 空态标题 */
  emptyTitle: '暂无数据',
  /** 错误态标题 */
  errorTitle: '加载失败',
  /** 错误态描述兜底（后端没给 msg 时） */
  errorDescription: '请求没有成功，请点「重试」再试一次；多次失败请联系管理员',
  /** 错误态主按钮 */
  retry: '重试',
} as const;

/** 描述文本上限（超出截断） */
export const STATE_VIEW_DESCRIPTION_MAX = 240;
/** 骨架屏行数下限 */
export const STATE_VIEW_ROWS_MIN = 1;
/** 骨架屏行数上限 */
export const STATE_VIEW_ROWS_MAX = 12;
/** 骨架屏默认行数 */
export const STATE_VIEW_ROWS_DEFAULT = 4;

const STATES: readonly StateViewState[] = ['loading', 'empty', 'error'];
const EMPTY_TYPES: readonly StateViewEmptyType[] = ['empty', 'success', 'fail', 'network-error', 'maintenance'];
const ERROR_TYPES: readonly StateViewEmptyType[] = ['fail', 'maintenance', 'network-error'];

/** 单行化：折叠空白 + 去首尾 + 超长截断（不认识的值当空串） */
export function oneLineText(text: unknown, max: number = STATE_VIEW_DESCRIPTION_MAX): string {
  const raw = typeof text === 'string' ? text : text === null || text === undefined ? '' : String(text);
  const flat = raw.replace(/\s+/g, ' ').trim();
  if (flat.length <= max) {
    return flat;
  }
  return `${flat.slice(0, Math.max(0, max - 1))}…`;
}

/** 骨架屏的一行：**必须是对象**（TDesign 的 `rowCol` 项会走 `'width' in item`，传字符串直接抛 TypeError） */
export interface SkeletonCell {
  width: string;
  height?: string;
}

/**
 * 骨架屏行：整行铺满，末行收窄（像真表格的最后一条），行数钳在 1..12。
 *
 * 返回值形状不能改：`t-skeleton` 的 `rowCol` 每项是「一行」，行内的列是**对象**
 * （`{ width, height }`）。实测传 `'100%'` 这类字符串会在渲染期抛
 * `TypeError: Cannot use 'in' operator to search for 'width' in 100%`，
 * 整棵子树挂不上（页面白区）。见 `state-view.test.ts` 的形状断言。
 */
export function skeletonRowCol(rows: unknown): SkeletonCell[] {
  const parsed = typeof rows === 'number' ? rows : Number(rows);
  const count = Number.isFinite(parsed)
    ? Math.min(STATE_VIEW_ROWS_MAX, Math.max(STATE_VIEW_ROWS_MIN, Math.trunc(parsed)))
    : STATE_VIEW_ROWS_DEFAULT;
  const widths: SkeletonCell[] = [];
  for (let i = 0; i < count; i += 1) {
    if (count >= 3 && i === count - 1) {
      widths.push({ width: '58%' });
    } else if (count >= 4 && i % 3 === 2) {
      widths.push({ width: '82%' });
    } else {
      widths.push({ width: '100%' });
    }
  }
  return widths;
}

/**
 * 从任意错误对象里取一句人话。
 *
 * 认 `string` / `Error` / 后端 `ApiError`（`{ code, msg, message }` 形状），取不到就空串
 * （空串会让 error 态落到兜底描述，比在界面上显示 `[object Object]` 强）。
 */
export function errorMessageOf(error: unknown): string {
  if (error === null || error === undefined) {
    return '';
  }
  if (typeof error === 'string') {
    return oneLineText(error);
  }
  if (error instanceof Error) {
    return oneLineText(error.message);
  }
  const bag = error as { msg?: unknown; message?: unknown };
  if (typeof bag.msg === 'string') {
    return oneLineText(bag.msg);
  }
  if (typeof bag.message === 'string') {
    return oneLineText(bag.message);
  }
  return '';
}

/** 组件入参（全是页面能声明的） */
export interface StateViewInput {
  state: StateViewState | string;
  /** loading 形态，默认 skeleton */
  variant?: string;
  /** 骨架屏行数，默认 4 */
  rows?: number | string;
  /** empty / error 标题 */
  title?: string;
  /** empty / error 描述（error 不给则用兜底话术） */
  description?: string;
  /** 主操作文案：empty 不给不画按钮；error 不给默认「重试」；显式 '' 一律不画 */
  actionText?: string;
  /** 次操作文案 */
  secondaryActionText?: string;
  /** 插画类型（t-empty 的 type） */
  emptyType?: string;
}

/** 组件实际渲染用的解析结果 */
export interface StateViewResolved {
  kind: StateViewState;
  variant: StateViewLoadingVariant;
  rows: number;
  rowCol: SkeletonCell[];
  emptyType: StateViewEmptyType;
  title: string;
  description: string;
  actionText: string;
  secondaryActionText: string;
  /** 是否画主操作按钮 */
  showAction: boolean;
}

/** 入参 → 渲染判据（规则见文件头 5 条） */
export function resolveStateView(input: StateViewInput): StateViewResolved {
  const kind: StateViewState = STATES.includes(input.state as StateViewState)
    ? (input.state as StateViewState)
    : 'empty';
  const rowCol = skeletonRowCol(input.rows);
  const variant: StateViewLoadingVariant = input.variant === 'spinner' ? 'spinner' : 'skeleton';
  const secondaryActionText = oneLineText(input.secondaryActionText, 40);

  if (kind === 'loading') {
    return {
      kind,
      variant,
      rows: rowCol.length,
      rowCol,
      emptyType: 'empty',
      title: oneLineText(input.title, 40) || STATE_VIEW_TEXT.loading,
      description: oneLineText(input.description),
      actionText: '',
      secondaryActionText,
      showAction: false,
    };
  }

  if (kind === 'error') {
    // 显式空串 = 不可重试（如 403）；undefined = 默认「重试」
    const actionText =
      input.actionText === undefined ? STATE_VIEW_TEXT.retry : oneLineText(input.actionText, 20);
    const emptyType = ERROR_TYPES.includes(input.emptyType as StateViewEmptyType)
      ? (input.emptyType as StateViewEmptyType)
      : 'network-error';
    return {
      kind,
      variant,
      rows: rowCol.length,
      rowCol,
      emptyType,
      title: oneLineText(input.title, 40) || STATE_VIEW_TEXT.errorTitle,
      description: oneLineText(input.description) || STATE_VIEW_TEXT.errorDescription,
      actionText,
      secondaryActionText,
      showAction: actionText !== '',
    };
  }

  const actionText = oneLineText(input.actionText, 20);
  return {
    kind,
    variant,
    rows: rowCol.length,
    rowCol,
    emptyType: EMPTY_TYPES.includes(input.emptyType as StateViewEmptyType)
      ? (input.emptyType as StateViewEmptyType)
      : 'empty',
    title: oneLineText(input.title, 40) || STATE_VIEW_TEXT.emptyTitle,
    description: oneLineText(input.description),
    actionText,
    secondaryActionText,
    showAction: actionText !== '',
  };
}

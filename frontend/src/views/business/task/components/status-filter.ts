/**
 * 任务列表「状态勾选筛选」纯逻辑（T9-B）。
 *
 * 老大要求（2026-10-30）：状态筛选从下拉改成**勾选标签、可多选、紧贴表格上方**。
 * 勾选即查询，所以要保证这里的三支函数是**纯的**：不碰 Vue、不碰 axios，
 * 只做数组运算 —— 这样 `tests/unit/task-status-filter.test.ts` 能用 `tsx --test`
 * 直接测真代码（本仓惯例，见 `components/tree-layout.ts` / `warehouse-tree.ts`）。
 *
 * 与后端 T9-A 的契约：`GET /api/business/task/page?statuses=TODO,DOING`
 * —— 逗号分隔多值；**空 = 不筛**（因此空数组必须序列化成 `undefined`，
 * 不能是空串：空串会被后端当成「筛选一个空状态」）。
 */

/** 多值状态的逗号分隔符（与后端 T9-A 契约逐字一致） */
export const STATUSES_SEPARATOR = ',';

/**
 * 勾选 / 取消一个状态，返回**新数组**（不改入参）。
 *
 * 顺序语义：新勾的追加到尾部，取消的按值移除 —— 于是「先点进行中再点待开始」
 * 得到 `['DOING','TODO']`，请求串与用户点击顺序一致，便于排查。
 *
 * # 参数
 * - `list`：当前已勾选的状态值（只读，不会被修改）
 * - `value`：本次点击的状态值
 */
export function toggleStatusIn(list: readonly string[], value: string): string[] {
  const index = list.indexOf(value);
  if (index >= 0) {
    const next = list.slice();
    next.splice(index, 1);
    return next;
  }
  return [...list, value];
}

/** 是否处于「有勾选」态（空数组 = 不筛） */
export function hasStatusFilter(list: readonly string[]): boolean {
  return list.length > 0;
}

/**
 * 清空勾选。
 *
 * 返回空数组而不是让调用方写 `query.statuses = []`：单测要覆盖「任意 → 空」
 * 这条行为，而直接给 reactive 属性赋字面量没法在纯逻辑层被断言。
 */
export function clearStatusList(): string[] {
  return [];
}

/**
 * 状态数组 → 请求参数 `statuses`。
 *
 * - 有勾选：`['TODO','DOING']` → `'TODO,DOING'`
 * - 无勾选：`[]` → `undefined`（axios 会丢掉 undefined，等于不传该参数 = 不筛）
 */
export function statusesParam(list: readonly string[]): string | undefined {
  if (list.length === 0) return undefined;
  return list.join(STATUSES_SEPARATOR);
}

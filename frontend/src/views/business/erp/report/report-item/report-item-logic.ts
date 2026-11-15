/**
 * 报表项页的纯逻辑 —— 不碰 DOM、不碰请求，可被 `tsx --test` 直接加载。
 *
 * 这里只放一件事：**「可见行下标」≠「全表下标」时的排序换算**。
 *
 * 背景：页面表格 `:data="visibleRows"`，而 `visibleRows` 是本地过滤
 * （keyword / statusFilter / onlyUnmatched）后的**子集**；表格 slot 给的 `rowIndex`
 * 是子集下标。但排序接口要的是**整表顺序**（公式只能引用排在它前面的项，后端整表校验），
 * 所以必须拿「被点的那一行」在 `rows` 里的位置去 splice。
 *
 * 曾经的写法是 `next.splice(rowIndex, 1)`：开着任一筛选时它会移动**另一行**，
 * 而提交给后端的 id 序列仍是整表全集（长度对、无重复），后端不会报错 ——
 * 「静默移错行」，比 400 难发现得多。
 *
 * 口径（`tests/unit/report-item-sort.test.ts` 逐条钉住）：
 * - 上下移动是**相邻可见行之间**的交换：↑ 移到上一个可见行前面，↓ 移到下一个可见行后面；
 *   中间夹着的不可见行原样保持在原地相对位置。
 * - 入口收的是**行 id**（不是下标），下标一律内部按 id 查 —— 下标太容易传错。
 * - 移不动（首行 ↑ / 末行 ↓ / 行不在表里）返回 `null`，调用方按「无操作」处理（不发请求）。
 */

/** 参与排序的行至少要有稳定主键（表格 `row-key="id"`）。 */
export interface SortableRow {
  id: string;
}

/** 全表下标；行不在表里返回 `-1`（不抛错，调用方按「无操作」处理）。 */
export function fullIndexOf<T extends SortableRow>(rows: readonly T[], id: string): number {
  return rows.findIndex((row) => row.id === id);
}

/** 可见位置上还能不能往 `delta` 方向挪一格（首行 ↑ / 末行 ↓ 为 false）。 */
export function canMoveVisible(visibleIndex: number, visibleCount: number, delta: number): boolean {
  if (visibleIndex < 0 || visibleIndex >= visibleCount) {
    return false;
  }
  const target = visibleIndex + (delta < 0 ? -1 : 1);
  return target >= 0 && target < visibleCount;
}

/**
 * 按「可见行的相邻关系」重排**整表**，返回新顺序与被移动的行。
 *
 * @param rows        全表（后端顺序，`sort_order` 升序）
 * @param visibleRows 当前过滤后的可见行（必须是 `rows` 的子序列，页面保证）
 * @param clickedId   被点 ↑↓ 的那一行的 id（表格 `rowIndex` 对应的行）
 * @param delta       `-1` 上移 / `1` 下移（符号有效，绝对值不算步数）
 * @returns `{ next, moved }`；移不动时 `null`
 */
export function moveVisibleRow<T extends SortableRow>(
  rows: readonly T[],
  visibleRows: readonly T[],
  clickedId: string,
  delta: number,
): { next: T[]; moved: T } | null {
  const step = delta < 0 ? -1 : 1;
  const visibleIndex = visibleRows.findIndex((row) => row.id === clickedId);
  if (!canMoveVisible(visibleIndex, visibleRows.length, step)) {
    return null;
  }
  const targetId = visibleRows[visibleIndex + step].id;
  const from = fullIndexOf(rows, clickedId);
  if (from < 0) {
    return null;
  }

  const next = rows.slice();
  const [moved] = next.splice(from, 1);
  // 目标行在「移除被移动行之后」的新下标：上移插它前面，下移插它后面。
  // 直接拿移除前的下标插入会在下移时差一格（下移方向移除后目标整体前移 1）。
  const anchor = next.findIndex((row) => row.id === targetId);
  if (anchor < 0) {
    return null;
  }
  next.splice(step > 0 ? anchor + 1 : anchor, 0, moved);
  return { next, moved };
}

/** 提交给后端的 id 序列：整表顺序，长度恒等于 `rows.length`。 */
export function sortPayloadIds<T extends SortableRow>(rows: readonly T[]): string[] {
  return rows.map((row) => row.id);
}

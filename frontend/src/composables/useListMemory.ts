/**
 * 列表记忆（O4）：记住列表页的筛选条件 + 页码 + 每页条数 + 排序 + 当前 Tab。
 *
 * 存储用 `sessionStorage`：**关掉标签页就清空**。
 * 直觉上「筛选」是这一次浏览的临时上下文，跨天重开还挂着上次的筛选会让人以为数据没了；
 * 而「表单记忆」（{@link useFormMemory}）是长期习惯，所以走 `localStorage`。
 *
 * 这里只做存取（纯函数，可单测）；把记忆接到页面上的粘合逻辑在
 * `useListFilterMemory.ts`（防抖写、恢复提示、一键清空）。
 */
import {
  readJson,
  removeKey,
  resolveStorage,
  writeJson,
  type MemoryLike,
} from './memory-core';

/** 每个列表的记忆 key 前缀 */
export const LIST_MEMORY_PREFIX = 'memory:list:';

/** 列表页要记的东西（字段都可缺省，写多少记多少） */
export interface ListMemoryState {
  /** 筛选字段（只记页面 `filter` 里真实存在的键） */
  filter: Record<string, unknown>;
  /** 当前页码（1 起） */
  page: number;
  /** 每页条数 */
  pageSize?: number;
  /** 排序字段与方向（当前 6 个模块都没开列表排序，留口子给以后） */
  sort?: { field: string; order: 'asc' | 'desc' } | null;
  /** 当前 Tab / 视图（任务页：list / board / burndown） */
  tab?: string;
}

export interface ListMemoryHandle {
  key: string;
  /** 读记忆（没有 / 损坏 → `{}`） */
  recall: () => Partial<ListMemoryState>;
  /** 写记忆（整份覆盖） */
  remember: (state: Partial<ListMemoryState>) => void;
  /** 清空（「清空筛选」按钮） */
  forget: () => void;
  /** 是否有记忆 */
  hasMemory: () => boolean;
}

export function useListMemory(listName: string, storage?: MemoryLike): ListMemoryHandle {
  const box = storage ?? resolveStorage('session');
  const key = `${LIST_MEMORY_PREFIX}${listName}`;

  const recall = (): Partial<ListMemoryState> => readJson<ListMemoryState>(box, key) ?? {};

  const remember = (state: Partial<ListMemoryState>): void => {
    writeJson(box, key, state as Record<string, unknown>);
  };

  const forget = (): void => removeKey(box, key);

  const hasMemory = (): boolean => Object.keys(recall()).length > 0;

  return { key, recall, remember, forget, hasMemory };
}

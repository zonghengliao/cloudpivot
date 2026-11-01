/**
 * 把「列表记忆」（{@link useListMemory}）接到页面上的粘合层 —— 6 个模块的列表页共用。
 *
 * 一句话：**进页面恢复筛选和页码，改了防抖写回，顶部给个可感知的提示 + 一键清空**。
 *
 * # 为什么单独一层
 *
 * 六个列表页的 `filter` / `current` / `loadXxx()` 形状几乎一样，但每页各自
 * `resetFilter` 不同。把「恢复 / 防抖写 / 提示文案 / 清空」收在这里，
 * 页面里只剩 3 行：建实例、`onMounted` 用 `current.value` 首次加载、模板挂提示条。
 *
 * # 三个容易踩的点
 *
 * 1. **恢复必须早于首次加载**：构造函数里就地恢复（setup 阶段），
 *    `onMounted` 再 `loadXxx(current.value)` —— 否则首屏永远是第 1 页全量。
 * 2. **只恢复当前 `filter` 真有的键**：老版本 sessionStorage 里可能留着已删字段，
 *    无脑 `Object.assign(filter, saved)` 会往 reactive 对象里塞幽灵键（提交时还可能被带上）。
 * 3. **没有有效筛选就删 key**：否则下次进页面会「恢复了 0 项」还挂个提示条，
 *    用户点「清空筛选」也没东西可清。
 */
import { computed, onScopeDispose, ref, watch, type ComputedRef, type Ref } from 'vue';
import { isBlank, type MemoryLike } from './memory-core';
import { useListMemory, type ListMemoryState } from './useListMemory';

/**
 * 「额外记忆字段」：值不在 `filter` 对象里的状态（如任务页独立成 ref 的截止日期范围）。
 * `read` 取当前值，`write` 把恢复值写回去。
 */
export interface ListMemoryExtraField {
  read: () => unknown;
  write: (value: unknown) => void;
}

export interface UseListFilterMemoryOptions<F extends object> {
  /** 列表名（key = `memory:list:<listName>`，一页一个，别复用） */
  listName: string;
  /** 筛选行（reactive 对象，就地读写） */
  filter: F;
  /** 当前页码（1 起） */
  page: Ref<number>;
  /** 每页条数（常量或 ref） */
  pageSize?: number | Ref<number>;
  /** 当前 Tab / 视图（任务页需要） */
  tab?: Ref<string>;
  /** 视为「默认、不必记」的 Tab 值 */
  defaultTab?: string;
  /** 允许恢复的 Tab 值（防止老记忆把视图切成已下线的那个） */
  allowedTabs?: readonly string[];
  /** 不在 `filter` 里的额外状态（键名即存储里的键名，别和 filter 撞名） */
  extra?: Record<string, ListMemoryExtraField>;
  /** 写记忆防抖（毫秒，默认 300 —— 别每次按键都写一次 sessionStorage） */
  debounceMs?: number;
  /** 存储（默认 `sessionStorage`；单测注入假实现） */
  storage?: MemoryLike;
}

export interface ListFilterMemoryHandle {
  /** 进页面时是否恢复过（提示条显隐） */
  restored: Ref<boolean>;
  /** 提示条文案（含恢复细节，或「已记住」） */
  restoredText: ComputedRef<string>;
  /** 用户改过筛选（文案从「已恢复」切到「已记住」） */
  dirty: Ref<boolean>;
  /** 立刻写一次记忆（防抖未到点时强制落盘） */
  persist: () => void;
  /** 清空记忆并收起提示条（「清空筛选」用） */
  forget: () => void;
}

/** 一份筛选里「有值」的键（空白值不算，空数组不算） */
export function activeKeysOf(filter: Record<string, unknown>): string[] {
  return Object.keys(filter).filter((key) => !isBlank(filter[key]));
}

export function useListFilterMemory<F extends object>(
  options: UseListFilterMemoryOptions<F>,
): ListFilterMemoryHandle {
  const {
    listName,
    filter,
    page,
    pageSize,
    tab,
    defaultTab = '',
    allowedTabs,
    extra,
    debounceMs = 300,
  } = options;

  const store = useListMemory(listName, options.storage);
  const restored = ref(false);
  const dirty = ref(false);
  /** 恢复时的细节（「3 项筛选 · 第 2 页」），提示条文案用 */
  const restoredDetail = ref('');

  // ==================== ① 恢复（必须先于首次加载） ====================
  const saved = store.recall();
  const savedFilter = (saved.filter ?? {}) as Record<string, unknown>;
  let restoredFilterCount = 0;
  for (const key of Object.keys(filter)) {
    const value = savedFilter[key];
    if (value === undefined) {
      continue;
    }
    (filter as Record<string, unknown>)[key] = value;
    if (!isBlank(value)) {
      restoredFilterCount += 1;
    }
  }

  const restoredPage = typeof saved.page === 'number' && saved.page > 1 ? Math.floor(saved.page) : 1;
  if (restoredPage > 1) {
    page.value = restoredPage;
  }

  if (pageSize && typeof pageSize !== 'number' && typeof saved.pageSize === 'number' && saved.pageSize > 0) {
    pageSize.value = saved.pageSize;
  }

  const savedTab = typeof saved.tab === 'string' ? saved.tab : '';
  const tabRestorable =
    Boolean(tab) &&
    savedTab !== '' &&
    savedTab !== defaultTab &&
    (!allowedTabs || allowedTabs.includes(savedTab));
  if (tabRestorable && tab) {
    tab.value = savedTab;
  }

  // 额外字段（不在 `filter` 里的状态，如任务的截止日期范围）
  let restoredExtraCount = 0;
  for (const [key, field] of Object.entries(extra ?? {})) {
    const value = savedFilter[key];
    if (value === undefined) {
      continue;
    }
    field.write(value);
    if (!isBlank(value)) {
      restoredExtraCount += 1;
    }
  }

  restored.value =
    restoredFilterCount > 0 || restoredExtraCount > 0 || restoredPage > 1 || tabRestorable;
  if (restored.value) {
    const parts: string[] = [];
    const filterTotal = restoredFilterCount + restoredExtraCount;
    if (filterTotal > 0) {
      parts.push(`${filterTotal} 项筛选`);
    }
    if (restoredPage > 1) {
      parts.push(`第 ${restoredPage} 页`);
    }
    if (tabRestorable) {
      parts.push(`视图：${savedTab}`);
    }
    restoredDetail.value = parts.join(' · ');
  }

  // ==================== ② 写（防抖） ====================
  let timer: ReturnType<typeof setTimeout> | null = null;

  const buildState = (): Partial<ListMemoryState> => {
    const filterState: Record<string, unknown> = {};
    for (const key of Object.keys(filter)) {
      const value = (filter as Record<string, unknown>)[key];
      if (!isBlank(value)) {
        filterState[key] = value;
      }
    }
    // 额外字段与筛选同层存（恢复时按同名键回写）
    for (const [key, field] of Object.entries(extra ?? {})) {
      const value = field.read();
      if (!isBlank(value)) {
        filterState[key] = value;
      }
    }
    const state: Partial<ListMemoryState> = { filter: filterState, page: page.value };
    const size = typeof pageSize === 'number' ? pageSize : pageSize?.value;
    if (typeof size === 'number' && size > 0) {
      state.pageSize = size;
    }
    const tabValue = tab?.value ?? '';
    if (tabValue && tabValue !== defaultTab) {
      state.tab = tabValue;
    }
    return state;
  };

  const isMeaningful = (state: Partial<ListMemoryState>): boolean =>
    Object.keys(state.filter ?? {}).length > 0 || (state.page ?? 1) > 1 || Boolean(state.tab);

  const persist = (): void => {
    if (timer) {
      clearTimeout(timer);
      timer = null;
    }
    const state = buildState();
    if (!isMeaningful(state)) {
      // 已经清空了 → 删 key，下次进页面别挂个「恢复了 0 项」的空提示
      store.forget();
      restored.value = false;
      restoredDetail.value = '';
      return;
    }
    store.remember(state);
  };

  const schedulePersist = (): void => {
    dirty.value = true;
    if (timer) {
      clearTimeout(timer);
    }
    timer = setTimeout(() => {
      timer = null;
      persist();
    }, debounceMs);
  };

  watch(
    () => {
      const extraValues: Record<string, unknown> = {};
      for (const [key, field] of Object.entries(extra ?? {})) {
        extraValues[key] = field.read();
      }
      return {
        ...(filter as Record<string, unknown>),
        __page: page.value,
        __tab: tab?.value,
        ...extraValues,
      };
    },
    schedulePersist,
  );

  // 离开页面时把没落盘的改动立刻写掉（防抖窗口内跳走不该丢筛选）
  onScopeDispose(() => {
    if (timer) {
      persist();
    }
  });

  const forget = (): void => {
    if (timer) {
      clearTimeout(timer);
      timer = null;
    }
    store.forget();
    restored.value = false;
    restoredDetail.value = '';
  };

  const restoredText = computed(() => {
    if (!restored.value) {
      return '';
    }
    if (dirty.value) {
      return '筛选条件已记住 · 翻页 / 进详情再回来都还在';
    }
    return restoredDetail.value ? `已恢复上次筛选条件（${restoredDetail.value}）` : '已恢复上次筛选条件';
  });

  return { restored, restoredText, dirty, persist, forget };
}

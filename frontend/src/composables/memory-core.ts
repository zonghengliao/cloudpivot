/**
 * 「记忆」能力的地基：纯函数 + 可注入的存储抽象。
 *
 * # 为什么先有这一层
 *
 * `useFormMemory` / `useListMemory` 要能被单测直接调用，而单测跑在
 * `tsx --test`（node，没有 DOM、没有 `localStorage`）。所以：
 *
 * - 所有读写都过 {@link MemoryLike}（只要求 `getItem/setItem/removeItem`），
 *   浏览器里默认 `window.localStorage` / `window.sessionStorage`，单测里塞假存储；
 * - 存储整体不可用（SSR、隐私模式、配额满、无 DOM）时退化成内存实现，
 *   页面照常跑，只是记忆不跨刷新 —— 绝不因为记忆功能把主流程带崩。
 *
 * # 空值语义（`isBlank`）
 *
 * 只把 `undefined` / `null` / `''` 当「没填」。
 * `0`（0 天账期、0% 税率）和 `false`（发票未随货）都是**真实选择**，
 * 必须能记住、也必须能覆盖旧记忆 —— 这是「空值不覆盖」铁律的唯一例外边界。
 */

/** 记忆存储的最小接口（`localStorage` / `sessionStorage` 天然满足） */
export interface MemoryLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

/** 存储不可用时的内存替身：本次会话内仍能读写，只是不跨刷新 */
export function createMemoryFallback(): MemoryLike {
  const bucket = new Map<string, string>();
  return {
    getItem: (key: string) => bucket.get(key) ?? null,
    setItem: (key: string, value: string) => {
      bucket.set(key, value);
    },
    removeItem: (key: string) => {
      bucket.delete(key);
    },
  };
}

/**
 * 取浏览器存储；拿不到（无 DOM / 被策略禁用 / 访问即抛）就用内存替身。
 *
 * 注意 Safari 隐私模式下 `localStorage` 这个属性存在，但 `setItem` 才抛，
 * 所以这里只做「能不能拿到对象」的判断，写入侧的 `try/catch` 在 {@link writeJson}。
 */
export function resolveStorage(kind: 'local' | 'session'): MemoryLike {
  try {
    const store = kind === 'local' ? globalThis.localStorage : globalThis.sessionStorage;
    if (store && typeof store.getItem === 'function') {
      return store;
    }
  } catch {
    // 访问 `localStorage` 本身抛异常（沙箱 / 权限策略）→ 退化
  }
  return createMemoryFallback();
}

/**
 * 是否「没填」：`undefined` / `null` / `''` / 空数组算空，`0` 与 `false` 不算。
 *
 * 空数组也算空是为了任务页的多选筛选（`statuses: []` 表示「没勾」，
 * 不该被当成一条有效筛选记进记忆，更不该冲掉上次勾的那几个状态）。
 */
export function isBlank(value: unknown): boolean {
  if (value === undefined || value === null || value === '') {
    return true;
  }
  return Array.isArray(value) && value.length === 0;
}

/**
 * 读一份 JSON 对象。
 *
 * 任何异常（无值、空串、`JSON.parse` 失败、存的是数组 / 标量）一律返回 `null`，
 * 调用方按「没有记忆」处理 —— 坏数据只该让记忆失效，不该让页面白屏。
 */
export function readJson<T extends object>(
  storage: MemoryLike,
  key: string,
): Partial<T> | null {
  try {
    const raw = storage.getItem(key);
    if (!raw) {
      return null;
    }
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
      return null;
    }
    return parsed as Partial<T>;
  } catch {
    return null;
  }
}

/** 写一份 JSON；写不进（配额满 / 被禁用）就静默放弃，主流程不受影响 */
export function writeJson(storage: MemoryLike, key: string, value: Record<string, unknown>): void {
  try {
    storage.setItem(key, JSON.stringify(value));
  } catch {
    // 记忆是锦上添花，写失败不该冒泡到业务
  }
}

/** 删一个键（同样静默） */
export function removeKey(storage: MemoryLike, key: string): void {
  try {
    storage.removeItem(key);
  } catch {
    // 同上
  }
}

/**
 * 只取指定字段里的非空值。
 *
 * 「只记指定字段」和「空值不覆盖」是同一条规则的两面：
 * 没选供应商就不该把上次选过的供应商冲掉。
 */
export function pickFields<T extends object>(
  source: Partial<T> | null | undefined,
  fields: readonly (keyof T)[],
): Partial<T> {
  const out: Partial<T> = {};
  if (!source) {
    return out;
  }
  for (const field of fields) {
    const value = source[field];
    if (!isBlank(value)) {
      out[field] = value;
    }
  }
  return out;
}

/**
 * 合并记忆：本次值覆盖旧值，但**空值不覆盖**（{@link isBlank} 口径）。
 *
 * 返回新对象，不改 `prev` / `next` 的引用 —— 调用方常常直接传 reactive 表单。
 */
export function mergeMemory<T extends object>(
  prev: Partial<T> | null | undefined,
  next: Partial<T> | null | undefined,
  fields: readonly (keyof T)[],
): Partial<T> {
  return { ...pickFields(prev, fields), ...pickFields(next, fields) };
}

/**
 * ★ 默认值优先级：**源单带出 > 上次记忆 > 系统默认**。
 *
 * - `base`：系统默认（`emptyXxxForm()`）；
 * - `memory`：localStorage 里的上次记忆；
 * - `source`：源单带出（如 D4 的采购 → 收货），只覆盖它**真正有值**的字段。
 *
 * 三层都按 `isBlank` 跳过空值：`source` 里没值的字段不会把记忆值打成空。
 * 返回非记忆字段（id、日期、明细…）保持 `base` 原值。
 */
export function resolveDefaults<T extends object>(
  base: T,
  memory: Partial<T> | null | undefined,
  source: Partial<T> | null | undefined,
  fields: readonly (keyof T)[],
): T {
  return {
    ...base,
    ...pickFields(memory, fields),
    ...pickFields(source, fields),
  };
}

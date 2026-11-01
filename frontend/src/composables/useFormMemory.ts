/**
 * 表单记忆（O1）：记住「上次新建时用的那几个字段」，下次新建自动带出。
 *
 * 存储用 `localStorage`（要跨标签页、跨浏览器重启保留），
 * key = `memory:form:<formName>`。**只存 id / 枚举 / 数字档位，不存金额、不存 token**
 * —— 金额是一次性的业务数据，留着只会让下次新建带出错误的数字。
 *
 * 典型接法（`xxx/index.vue`）：
 *
 * ```ts
 * const formMemory = useFormMemory<PurchaseFormState>('erp-purchase', MEMORY_FIELDS, {
 *   labels: { supplier_id: '供应商', warehouse_id: '仓库' },
 * });
 * // 新建：系统默认 → 记忆 → 源单（源单最后盖，优先级最高）
 * Object.assign(form, resolveDefaults(emptyPurchaseForm(), formMemory.recall(), source));
 * // 保存成功后：记住这一次
 * formMemory.remember(toRaw(form));
 * ```
 */
import {
  isBlank,
  mergeMemory,
  pickFields,
  readJson,
  removeKey,
  resolveStorage,
  writeJson,
  type MemoryLike,
} from './memory-core';

/** 每个表单的记忆 key 前缀（改前缀 = 让所有用户重新记一次，慎改） */
export const FORM_MEMORY_PREFIX = 'memory:form:';

export interface FormMemoryOptions {
  /** 存储（默认 `localStorage`；单测注入假实现） */
  storage?: MemoryLike;
  /** 字段中文名（提示条文案用，缺省回落到字段名本身） */
  labels?: Record<string, string>;
}

export interface FormMemoryHandle<T extends object> {
  /** 存储 key（调试 / 排查用） */
  key: string;
  /** 读上次记忆（没有 / 损坏 → `{}`，不抛） */
  recall: () => Partial<T>;
  /** 记住本次值（只写 `fields` 里的非空值；空值不覆盖已有记忆） */
  remember: (form: Partial<T>) => void;
  /** 清空记忆（提示条上的「重置」） */
  forget: () => void;
  /** 是否有任何一条记忆（决定提示条显不显示） */
  hasMemory: () => boolean;
  /**
   * 这次新建实际「带出了」哪几个字段的中文名 —— 提示条文案用。
   *
   * `source`（源单带出）里已有值的字段不算「记忆带出」：值是源单的，
   * 说是记忆带出的会误导用户。
   */
  labelsOf: (source?: Partial<T>) => string[];
}

export function useFormMemory<T extends object>(
  formName: string,
  fields: readonly (keyof T)[],
  options: FormMemoryOptions = {},
): FormMemoryHandle<T> {
  const storage = options.storage ?? resolveStorage('local');
  const labels = options.labels ?? {};
  const key = `${FORM_MEMORY_PREFIX}${formName}`;

  const recall = (): Partial<T> => readJson<T>(storage, key) ?? {};

  const remember = (form: Partial<T>): void => {
    const next = mergeMemory<T>(recall(), form, fields);
    if (Object.keys(next).length === 0) {
      // 一个非空值都没有 → 等价于没有记忆，别留个 `{}` 让提示条误报
      removeKey(storage, key);
      return;
    }
    writeJson(storage, key, next as Record<string, unknown>);
  };

  const forget = (): void => removeKey(storage, key);

  const hasMemory = (): boolean => Object.keys(pickFields<T>(recall(), fields)).length > 0;

  const labelsOf = (source: Partial<T> = {}): string[] => {
    const memory = pickFields<T>(recall(), fields);
    const fromSource = pickFields<T>(source, fields);
    return fields
      .filter((field) => !isBlank(memory[field]) || !isBlank(fromSource[field]))
      .filter((field) => isBlank(fromSource[field]))
      .map((field) => labels[String(field)] ?? String(field));
  };

  return { key, recall, remember, forget, hasMemory, labelsOf };
}

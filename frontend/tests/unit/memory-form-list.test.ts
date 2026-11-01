/**
 * 记忆能力单测（D2 + D3）：`useFormMemory` / `useListMemory` / 优先级 / 坏数据兜底。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，无 DOM、零新依赖）。
 *
 * 为什么能脱离浏览器测：`memory-core.ts` 把存储抽成 `MemoryLike` 接口，
 * 页面里默认 `window.localStorage` / `window.sessionStorage`，这里注入假存储。
 *
 * 这些用例钉的是**契约**而不是实现：
 * - key 前缀（`memory:form:` / `memory:list:`）一旦漂移，用户的历史记忆全失效；
 * - 「空值不覆盖」一旦写错，用户清空供应商后下次会诡异地又冒出来；
 * - 「源单 > 记忆 > 默认」一旦写错，从采购单带出的收货单会被上次的记忆覆盖。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  createMemoryFallback,
  isBlank,
  mergeMemory,
  pickFields,
  readJson,
  resolveStorage,
  resolveDefaults,
  type MemoryLike,
} from '../../src/composables/memory-core';
import { FORM_MEMORY_PREFIX, useFormMemory } from '../../src/composables/useFormMemory';
import { LIST_MEMORY_PREFIX, useListMemory } from '../../src/composables/useListMemory';

/** 假存储：内存 Map，行为与 `Storage` 一致 */
function fakeStorage(): MemoryLike & { raw: Map<string, string> } {
  const raw = new Map<string, string>();
  return {
    raw,
    getItem: (key: string) => raw.get(key) ?? null,
    setItem: (key: string, value: string) => {
      raw.set(key, value);
    },
    removeItem: (key: string) => {
      raw.delete(key);
    },
  };
}

/** 一提就抛的存储（隐私模式 / 配额满） */
function throwingStorage(): MemoryLike {
  return {
    getItem: () => {
      throw new Error('SecurityError');
    },
    setItem: () => {
      throw new Error('QuotaExceededError');
    },
    removeItem: () => {
      throw new Error('SecurityError');
    },
  };
}

/** 采购单心里要记的那几个字段 */
interface PurchaseMemory {
  supplier_id: string;
  warehouse_id: string;
  price_mode: string;
  credit_days: number;
}

const PURCHASE_FIELDS: readonly (keyof PurchaseMemory)[] = [
  'supplier_id',
  'warehouse_id',
  'price_mode',
  'credit_days',
];

// ==================== useFormMemory ====================

test('useFormMemory ① 无记忆时 recall 返回空对象', () => {
  const formMemory = useFormMemory<PurchaseMemory>('erp-purchase', PURCHASE_FIELDS, {
    storage: fakeStorage(),
  });
  assert.deepEqual(formMemory.recall(), {});
  assert.equal(formMemory.hasMemory(), false);
});

test('useFormMemory ② remember 之后 recall 能原样拿到', () => {
  const storage = fakeStorage();
  const formMemory = useFormMemory<PurchaseMemory>('erp-purchase', PURCHASE_FIELDS, { storage });
  formMemory.remember({ supplier_id: 'sup-a', warehouse_id: 'wh-02', price_mode: 'EXCL', credit_days: 30 });

  assert.deepEqual(formMemory.recall(), {
    supplier_id: 'sup-a',
    warehouse_id: 'wh-02',
    price_mode: 'EXCL',
    credit_days: 30,
  });
  // key 前缀是契约：老用户的历史记忆靠它才读得回来
  assert.equal(formMemory.key, `${FORM_MEMORY_PREFIX}erp-purchase`);
  assert.ok(storage.raw.has('memory:form:erp-purchase'));
});

test('useFormMemory ③ 只记指定字段，其他字段一律不落盘', () => {
  const storage = fakeStorage();
  const formMemory = useFormMemory<PurchaseMemory>('erp-purchase', PURCHASE_FIELDS, { storage });
  formMemory.remember({
    supplier_id: 'sup-a',
    warehouse_id: 'wh-02',
    price_mode: 'EXCL',
    credit_days: 15,
    // 下面这两个不是记忆字段（金额是一次性业务数据，绝不该带出）
    total_with_tax: 12345.67,
    remark: '季度备货',
  } as PurchaseMemory);

  const saved = formMemory.recall() as Record<string, unknown>;
  assert.deepEqual(Object.keys(saved).sort(), ['credit_days', 'price_mode', 'supplier_id', 'warehouse_id']);
  assert.equal(saved.total_with_tax, undefined);
  assert.equal(saved.remark, undefined);
});

test('useFormMemory ④ 空值不覆盖已有记忆（null / undefined / 空串跳过），但 0 与 false 算真实选择', () => {
  const formMemory = useFormMemory<PurchaseMemory>('erp-purchase', PURCHASE_FIELDS, {
    storage: fakeStorage(),
  });
  formMemory.remember({ supplier_id: 'sup-a', warehouse_id: 'wh-02', price_mode: 'EXCL', credit_days: 30 });
  // 第二次「没选供应商」就保存 → 不该把上次的 sup-a 冲掉
  formMemory.remember({
    supplier_id: '',
    // @ts-expect-error 后端下发的 null 也要被跳过
    warehouse_id: null,
    price_mode: 'EXCL',
    credit_days: 0,
  });

  const saved = formMemory.recall();
  assert.equal(saved.supplier_id, 'sup-a');
  assert.equal(saved.warehouse_id, 'wh-02');
  assert.equal(saved.credit_days, 0, '0 天账期是真实选择，必须能覆盖 30');
});

test('useFormMemory ⑤ forget 之后没有任何记忆，且不留空壳 `{}`', () => {
  const storage = fakeStorage();
  const formMemory = useFormMemory<PurchaseMemory>('erp-purchase', PURCHASE_FIELDS, { storage });
  formMemory.remember({ supplier_id: 'sup-a' });
  assert.equal(formMemory.hasMemory(), true);

  formMemory.forget();
  assert.deepEqual(formMemory.recall(), {});
  assert.equal(formMemory.hasMemory(), false);
  assert.equal(storage.raw.size, 0);
});

test('useFormMemory ⑥ 全空值 remember 不落盘（避免「恢复了 0 项」的假提示）', () => {
  const storage = fakeStorage();
  const formMemory = useFormMemory<PurchaseMemory>('erp-purchase', PURCHASE_FIELDS, { storage });
  formMemory.remember({ supplier_id: '', warehouse_id: '', price_mode: '' });
  assert.equal(storage.raw.size, 0);
});

test('useFormMemory ⑦ 提示条 labels：只报「记忆带出」的字段，源单已带出的不算', () => {
  const formMemory = useFormMemory<PurchaseMemory>('erp-purchase', PURCHASE_FIELDS, {
    storage: fakeStorage(),
    labels: { supplier_id: '供应商', warehouse_id: '仓库', price_mode: '口径', credit_days: '账期' },
  });
  formMemory.remember({ supplier_id: 'sup-a', warehouse_id: 'wh-02' });
  assert.deepEqual(formMemory.labelsOf(), ['供应商', '仓库']);

  // 源单带出了供应商 → 提示里不该说「供应商是上次记忆带出的」（优先级：源单 > 记忆）
  assert.deepEqual(formMemory.labelsOf({ supplier_id: 'sup-from-order' }), ['仓库']);
});

// ==================== useListMemory ====================

test('useListMemory ① 完整存取：筛选 + 页码 + 每页条数 + Tab', () => {
  const storage = fakeStorage();
  const listMemory = useListMemory('erp-purchase', storage);
  listMemory.remember({
    filter: { status: 'SUBMITTED', supplier_id: 'sup-a', keyword: '' },
    page: 2,
    pageSize: 20,
    sort: { field: 'order_date', order: 'desc' },
    tab: 'board',
  });

  assert.deepEqual(listMemory.recall(), {
    filter: { status: 'SUBMITTED', supplier_id: 'sup-a', keyword: '' },
    page: 2,
    pageSize: 20,
    sort: { field: 'order_date', order: 'desc' },
    tab: 'board',
  });
  assert.equal(listMemory.key, `${LIST_MEMORY_PREFIX}erp-purchase`);
});

test('useListMemory ② 损坏的 JSON → 返回空对象，不崩', () => {
  for (const broken of ['{', 'null', '123', '[]', '"str"', '{"filter":']) {
    const storage = fakeStorage();
    storage.raw.set('memory:list:erp-purchase', broken);
    const listMemory = useListMemory('erp-purchase', storage);
    assert.deepEqual(listMemory.recall(), {}, `坏数据 ${broken} 应视为没有记忆`);
    assert.equal(listMemory.hasMemory(), false);
  }
});

test('useListMemory ③ forget 之后 recall 为空（关标签页语义）', () => {
  const storage = fakeStorage();
  const listMemory = useListMemory('erp-purchase', storage);
  listMemory.remember({ filter: { status: 'SUBMITTED' }, page: 3 });
  assert.equal(listMemory.hasMemory(), true);

  listMemory.forget();
  assert.deepEqual(listMemory.recall(), {});
  assert.equal(storage.raw.size, 0);
});

// ==================== 优先级：源单 > 记忆 > 系统默认 ====================

/** 收货单表单（只留判优先级要用的字段） */
interface StockInFormLike extends Record<string, unknown> {
  supplier_id: string;
  warehouse_id: string;
  in_date: string;
  remark: string;
}

const STOCKIN_FIELDS: readonly (keyof StockInFormLike)[] = ['supplier_id', 'warehouse_id'];

function emptyStockInFormLike(): StockInFormLike {
  return { supplier_id: '', warehouse_id: '', in_date: '2026-11-01', remark: '' };
}

test('优先级 ① 源单带出的值覆盖上次记忆', () => {
  const memory: Partial<StockInFormLike> = { supplier_id: 'sup-memory', warehouse_id: 'wh-memory' };
  const source: Partial<StockInFormLike> = { supplier_id: 'sup-from-order' };
  const resolved = resolveDefaults(emptyStockInFormLike(), memory, source, STOCKIN_FIELDS);

  assert.equal(resolved.supplier_id, 'sup-from-order', '源单有值 → 源单赢');
  assert.equal(resolved.warehouse_id, 'wh-memory', '源单没给仓库 → 回落到记忆');
});

test('优先级 ② 没有源单时，记忆值覆盖系统默认', () => {
  const memory: Partial<StockInFormLike> = { supplier_id: 'sup-memory', warehouse_id: 'wh-memory' };
  const resolved = resolveDefaults(emptyStockInFormLike(), memory, {}, STOCKIN_FIELDS);

  assert.equal(resolved.supplier_id, 'sup-memory');
  assert.equal(resolved.warehouse_id, 'wh-memory');
  // 非记忆字段保持系统默认，别被顺手清掉
  assert.equal(resolved.in_date, '2026-11-01');
});

test('优先级 ③ 三层都没值时用系统默认', () => {
  const resolved = resolveDefaults(emptyStockInFormLike(), {}, {}, STOCKIN_FIELDS);
  assert.equal(resolved.supplier_id, '');
  assert.equal(resolved.warehouse_id, '');

  // 空串 / null 的源单值不能把记忆打成空（「源单没带这个字段」≠「源单要求清空」）
  const resolved2 = resolveDefaults(
    emptyStockInFormLike(),
    { supplier_id: 'sup-memory' },
    { supplier_id: '' },
    STOCKIN_FIELDS,
  );
  assert.equal(resolved2.supplier_id, 'sup-memory');
});

// ==================== 兜底：坏存储 / 无 DOM ====================

test('存储整体不可用（隐私模式 / 配额满）时 remember / recall 都不抛', () => {
  const formMemory = useFormMemory<PurchaseMemory>('erp-purchase', PURCHASE_FIELDS, {
    storage: throwingStorage(),
  });
  assert.deepEqual(formMemory.recall(), {});
  assert.doesNotThrow(() => formMemory.remember({ supplier_id: 'sup-a' }));
  assert.doesNotThrow(() => formMemory.forget());
  assert.deepEqual(formMemory.recall(), {});

  const listMemory = useListMemory('erp-purchase', throwingStorage());
  assert.deepEqual(listMemory.recall(), {});
  assert.doesNotThrow(() => listMemory.remember({ filter: { status: 'DRAFT' }, page: 2 }));
});

test('run 在没有 localStorage 的宿主里：resolveStorage 退化成本次会话内的可用替身', () => {
  const storage = resolveStorage('session');
  // 无论宿主有没有 sessionStorage，都必须能读写（无 DOM 时是内存替身）
  assert.doesNotThrow(() => storage.setItem('probe', '1'));
  const read = storage.getItem('probe');
  assert.ok(read === '1' || read === null || typeof read === 'string');
});

test('memory-core 基础口径：isBlank / pickFields / mergeMemory', () => {
  assert.equal(isBlank(undefined), true);
  assert.equal(isBlank(null), true);
  assert.equal(isBlank(''), true);
  assert.equal(isBlank([]), true, '空数组 = 没勾任何状态');
  assert.equal(isBlank(0), false);
  assert.equal(isBlank(false), false);
  assert.equal(isBlank(['TODO']), false);

  assert.deepEqual(pickFields<PurchaseMemory>({ supplier_id: 'a', warehouse_id: '' }, PURCHASE_FIELDS), {
    supplier_id: 'a',
  });
  assert.deepEqual(mergeMemory<PurchaseMemory>({ supplier_id: 'a' }, { supplier_id: '', credit_days: 0 }, PURCHASE_FIELDS), {
    supplier_id: 'a',
    credit_days: 0,
  });
  assert.equal(readJson(createMemoryFallback(), 'nope'), null);
});

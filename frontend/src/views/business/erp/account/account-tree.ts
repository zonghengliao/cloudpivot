/**
 * 会计科目页的**纯逻辑**（P4-C）——不 import vue，可被 `tsx --test` 直接单测。
 *
 * 拆出来的理由和 `warehouse-tree.ts` / `task/components/tree-layout.ts` 一样：
 * 「树怎么拼、类型怎么译名、过滤后为什么还能看见父节点」都是能用断言说清的事实，
 * `.vue` 只负责渲染和调接口。后端改枚举 / 改字段名时，`tests/unit/erp-finance-base.test.ts` 先红。
 *
 * # 三条与后端对齐的硬口径（`grep` + `psql \d` 佐证，不猜）
 *
 * 1. **科目类型只有 5 个字面量，且损益叫 `PROFIT_LOSS`**：
 *    `ck_biz_account_category` CHECK = `ASSET | LIABILITY | EQUITY | COST | PROFIT_LOSS`。
 *    卡面草稿写的 `PROFIT` 在库里不存在 —— 这里保留它作为**别名**宽容处理（旧数据 / 客户端拼错不炸），
 *    但下拉选项只给 5 个真值。
 * 2. **`parent_id` 自引用、`level` 是库列**（`ck_biz_account_level` ≥ 1）：
 *    树只能前端拼（后端没有 `/account/tree`，4 条科目不值得为它造一个接口）。
 * 3. **`status` 是 smallint 1/0**（不是布尔、不是字符串）：`1` 启用、`0` 停用。
 *
 * # 拼树的三个边界（单测逐条钉住）
 *
 * - **孤儿**：`parent_id` 指向一条被过滤掉 / 被软删的科目 → 该行按**根**渲染（不能整枝消失）；
 * - **自环**：`parent_id === id`（库里有 `ck_biz_account_no_self` 挡着，但前端不假设库永远干净）；
 * - **互指环**：A 的父是 B、B 的父是 A —— 摘链时把环上第一个节点当根，并**截断**该环的父指针。
 */

import type { AccountCategory, AccountRow, BalanceDirection } from '@/api/business/erp';

/** 建树后的节点：`AccountRow` + `children`（**叶子的 `children` 是 `undefined`**，不是空数组） */
export interface AccountTreeNode extends AccountRow {
  /** 下级科目（叶子没有这个字段 —— TDesign 树形表格靠「有没有 children」判箭头） */
  children?: AccountTreeNode[];
}

/** 每层缩进（px）。与任务列表 T8-B 同口径 24：一眼能看出层级，又不至于第 5 层跑出屏幕。 */
export const TREE_INDENT = 24;

/** 科目类型中文（键与 `ck_biz_account_category` 逐字一致；`PROFIT` 只是别名） */
const ACCOUNT_CATEGORY_LABELS: Record<string, string> = {
  ASSET: '资产',
  LIABILITY: '负债',
  EQUITY: '权益',
  COST: '成本',
  PROFIT_LOSS: '损益',
  PROFIT: '损益',
};

/** 科目类型 → 中文（未知值原样返回，宁可露出后端新枚举也不要显示空白） */
export function accountCategoryLabel(category: string | null | undefined): string {
  const key = (category ?? '').trim();
  if (!key) {
    return '—';
  }
  return ACCOUNT_CATEGORY_LABELS[key] ?? key;
}

/**
 * 科目类型 → `t-tag` 主题色。
 *
 * 只给真值 5 档（`PROFIT` 与 `PROFIT_LOSS` 同色），未知值兜底 `default`：
 * 资产=主色、负债=警告、权益=成功、成本=默认、损益=危险（与财务口径「损益波动看红」一致）。
 */
export function accountCategoryTheme(
  category: string | null | undefined,
): 'primary' | 'success' | 'warning' | 'danger' | 'default' {
  switch ((category ?? '').trim()) {
    case 'ASSET':
      return 'primary';
    case 'LIABILITY':
      return 'warning';
    case 'EQUITY':
      return 'success';
    case 'COST':
      return 'default';
    case 'PROFIT_LOSS':
    case 'PROFIT':
      return 'danger';
    default:
      return 'default';
  }
}

/** 科目类型下拉项（顺序照后端 CHECK 约束的书写顺序，不按中文拼音排） */
export const ACCOUNT_CATEGORY_OPTIONS: Array<{ value: AccountCategory; label: string }> = [
  { value: 'ASSET', label: '资产' },
  { value: 'LIABILITY', label: '负债' },
  { value: 'EQUITY', label: '权益' },
  { value: 'COST', label: '成本' },
  { value: 'PROFIT_LOSS', label: '损益' },
];

/** 余额方向 → 中文（`ck_biz_account_direction`：`DEBIT|CREDIT`） */
export function balanceDirectionLabel(direction: BalanceDirection | string | null | undefined): string {
  const key = (direction ?? '').trim();
  if (key === 'DEBIT') {
    return '借';
  }
  if (key === 'CREDIT') {
    return '贷';
  }
  return key || '—';
}

/** 启用状态 → 中文（`status` 是 smallint：1 启用 / 0 停用） */
export function accountStatusLabel(status: number | null | undefined): string {
  return status === 1 ? '启用' : '停用';
}

/** 启用状态 → `t-tag` 主题色 */
export function accountStatusTheme(status: number | null | undefined): 'success' | 'default' {
  return status === 1 ? 'success' : 'default';
}

/** 启用状态下拉项（`''` = 全部，由页面拼）；值用数字与后端 `status: Option<i16>` 对齐 */
export const ACCOUNT_STATUS_OPTIONS: Array<{ value: number; label: string }> = [
  { value: 1, label: '启用' },
  { value: 0, label: '停用' },
];

/** 层级 → 中文（会计科目常规到 3 级；超出就如实报数字，不编「四级」这种不存在的口径） */
export function accountLevelLabel(level: number | null | undefined): string {
  const value = typeof level === 'number' && Number.isFinite(level) ? level : 0;
  if (value <= 0) {
    return '—';
  }
  const names: Record<number, string> = { 1: '一级', 2: '二级', 3: '三级' };
  return names[value] ?? `${value} 级`;
}

// ==================== 排序 ====================

/**
 * 科目编码自然序比较（`1405` < `2202` < `2221` < `2221.01` < `2221.1.01`）。
 *
 * 为什么要自己写：`localeCompare` 在 `2221.01` vs `2221.1` 上会给出字典序
 * （`'2221.01' < '2221.1'` 恰好对，但 `'2221.02'` vs `'2221.1'` 就反了），
 * 财务看报表按「点号分段 + 段内数字」比。
 */
export function compareAccountCode(left: string, right: string): number {
  const a = left.split('.');
  const b = right.split('.');
  const len = Math.max(a.length, b.length);
  for (let index = 0; index < len; index += 1) {
    const segA = a[index] ?? '';
    const segB = b[index] ?? '';
    if (segA === segB) {
      continue;
    }
    const numA = Number(segA);
    const numB = Number(segB);
    const bothNumeric = Number.isFinite(numA) && Number.isFinite(numB) && segA !== '' && segB !== '';
    if (bothNumeric && numA !== numB) {
      return numA - numB;
    }
    // 一段数字一段字母（如 `6602` vs `6602A`）→ 字典序兜底
    return segA < segB ? -1 : 1;
  }
  return 0;
}

/** 按科目编码自然序排序（不改原数组） */
export function sortAccounts<T extends { account_code: string }>(rows: readonly T[]): T[] {
  return [...rows].sort((left, right) => compareAccountCode(left.account_code, right.account_code));
}

// ==================== 树 ====================

/**
 * 平铺 → 树。
 *
 * 入参是**一页科目**（页面把 `page_size` 给大值一次拉全），按 `parent_id` 挂到父节点下：
 * - 父在列表里 → 挂进去；
 * - 父不在列表里（孤儿 / 被过滤 / 被软删）→ **自己当根**（整枝不能消失）；
 * - `parent_id === id` 或互指成环 → **自己当根**，且不再向上找（`safeParentId` 负责摘链）。
 *
 * 子节点顺序 = 编码自然序（父在前、子的顺序照编码，符合财务习惯）。
 */
export function buildAccountTree(rows: readonly AccountRow[]): AccountTreeNode[] {
  const sorted = sortAccounts(rows);
  const byId = new Map<string, AccountTreeNode>();
  for (const row of sorted) {
    byId.set(row.id, { ...row });
  }

  const roots: AccountTreeNode[] = [];
  for (const row of sorted) {
    const node = byId.get(row.id);
    if (!node) {
      continue;
    }
    const parentId = safeParentId(row, byId);
    const parent = parentId ? byId.get(parentId) : undefined;
    if (parent && parent.id !== row.id) {
      parent.children = [...(parent.children ?? []), node];
    } else {
      roots.push(node);
    }
  }

  return pruneEmptyChildren(roots);
}

/**
 * 摘链：返回**可安全使用的父 id**。
 *
 * - 父不在本批数据里 → `null`（孤儿按根处理，不能整枝消失）；
 * - `parent_id === self` → `null`；
 * - 沿父链往上走，若回到自己（互指环）→ `null`（环上第一个节点当根，其余仍挂在它下面）。
 */
function safeParentId(row: AccountRow, byId: Map<string, AccountTreeNode>): string | null {
  const parentId = (row.parent_id ?? '').trim();
  if (!parentId || parentId === row.id || !byId.has(parentId)) {
    return null;
  }
  const seen = new Set<string>([row.id]);
  let cursor: string | null = parentId;
  // 上限 = 全表行数 + 1：正常树必然先结束，超过说明数据成环
  for (let guard = 0; guard <= byId.size; guard += 1) {
    if (!cursor) {
      return parentId;
    }
    if (seen.has(cursor)) {
      return null;
    }
    seen.add(cursor);
    cursor = (byId.get(cursor)?.parent_id ?? '').trim() || null;
  }
  return null;
}

/** 去掉空的 `children`（TDesign 树形表格把 `children: []` 也当「有下级」，会画出空箭头） */
function pruneEmptyChildren(nodes: AccountTreeNode[]): AccountTreeNode[] {
  return nodes.map((node) => {
    const children = pruneEmptyChildren(node.children ?? []);
    if (children.length === 0) {
      const { children: _drop, ...rest } = node;
      return rest;
    }
    return { ...node, children };
  });
}

/** 展平后的一行（带深度，方便断言「3 层结构」） */
export interface FlatAccount {
  /** 节点本体 */
  node: AccountTreeNode;
  /** 深度（1 起；与库列 `level` 不一定相等，孤儿会上浮到第 1 层） */
  depth: number;
}

/** 深度优先展平（父在前，子的顺序照编码） */
export function flattenAccountTree(nodes: AccountTreeNode[], depth = 1): FlatAccount[] {
  const out: FlatAccount[] = [];
  for (const node of nodes) {
    out.push({ node, depth });
    if (node.children?.length) {
      out.push(...flattenAccountTree(node.children, depth + 1));
    }
  }
  return out;
}

/** 树的最大深度（0 = 空树） */
export function accountTreeDepth(nodes: AccountTreeNode[]): number {
  return flattenAccountTree(nodes).reduce((max, row) => Math.max(max, row.depth), 0);
}

/** 科目总数（含各级子科目） */
export function countAccounts(nodes: AccountTreeNode[]): number {
  return flattenAccountTree(nodes).length;
}

/** 有下级的节点 id（= 能展开的节点），前序收集 */
export function expandableAccountIds(nodes: AccountTreeNode[]): string[] {
  const out: string[] = [];
  const walk = (list: AccountTreeNode[]): void => {
    for (const node of list) {
      if (node.children?.length) {
        out.push(node.id);
        walk(node.children);
      }
    }
  };
  walk(nodes);
  return out;
}

/**
 * 首屏默认展开：**全展开**。
 *
 * 会计科目是「少量、多层、看全貌」的数据（种子 4 条、企业户也就几百条到 3 级），
 * 收起父节点等于让用户多点两次才能确认 `2221` 下面有没有 `2221.01`。
 * 科目规模变大（上千条）时再改成「只展开第 1 层」，那时也是改这一个函数。
 */
export function initialExpandedAccountIds(nodes: AccountTreeNode[]): string[] {
  return expandableAccountIds(nodes);
}

/**
 * 逐节点独立折叠（`t-enhanced-table` 的受控展开态）。
 *
 * 只动自己这一个 id，不联级 —— 联级展开/收起交给页面的「全部展开 / 全部收起」按钮，
 * 两种意图分开，行为才可预期。
 */
export function toggleExpandedAccountId(expanded: readonly string[], id: string): string[] {
  return expanded.includes(id) ? expanded.filter((item) => item !== id) : [...expanded, id];
}

/** 节点 key（`row-key` 用 `id`；e2e mock 里 id 可能是空串，退到编码兜底，避免整表 key 撞车） */
export function accountNodeKey(row: { id?: string | null; account_code?: string | null }, index = 0): string {
  const id = (row.id ?? '').trim();
  if (id) {
    return id;
  }
  const code = (row.account_code ?? '').trim();
  return code ? `code:${code}` : `account:#${index}`;
}

/** 整棵树的 `row-key` 列表（保证互不相同，重复的补 `~序号` 后缀） */
export function accountTreeKeys(nodes: AccountTreeNode[]): string[] {
  const seen = new Map<string, number>();
  return flattenAccountTree(nodes).map(({ node }, index) => {
    const base = accountNodeKey(node, index);
    const hit = seen.get(base) ?? 0;
    seen.set(base, hit + 1);
    return hit === 0 ? base : `${base}~${hit}`;
  });
}

// ==================== 过滤（关键词 / 类型 / 状态） ====================

/** 页面筛选状态（`''` = 全部） */
export interface AccountFilterState {
  /** 关键词（编码 / 名称，前端模糊，大小写不敏感） */
  keyword: string;
  /** 科目类型 */
  account_category: AccountCategory | '';
  /** 启用状态（`null` = 全部；数字与后端 `status` 对齐） */
  status: number | null;
}

/** 空筛选 */
export function emptyAccountFilter(): AccountFilterState {
  return { keyword: '', account_category: '', status: null };
}

/** 筛选是否有内容（决定「重置」按钮是否可用 / 表头提示是否写「已过滤」） */
export function isAccountFiltered(filter: AccountFilterState): boolean {
  return filter.keyword.trim() !== '' || filter.account_category !== '' || filter.status !== null;
}

/** 过滤结果 */
export interface AccountFilterResult {
  /** 过滤后的树（命中节点 + 它们的祖先） */
  tree: AccountTreeNode[];
  /** 需要自动展开的 id（前序，祖先在前） */
  expandedIds: string[];
  /** 命中个数（含子孙命中的父节点不算命中） */
  matchedCount: number;
}

/**
 * 按关键词 / 类型 / 状态过滤科目树。
 *
 * **必须保留命中的节点 + 它的全部祖先**：只留命中节点的话，命中 `2221.01` 时父科目
 * `2221` 被剪掉，整枝会在界面上「静默消失」（`warehouse-tree.ts` 记的同一个坑）。
 * 祖先**不算命中**（`matchedCount` 不涨），否则「搜 2221.01 显示命中 2 条」会让人以为搜错了。
 */
export function filterAccountTree(nodes: AccountTreeNode[], filter: AccountFilterState): AccountFilterResult {
  const needle = filter.keyword.trim().toLowerCase();
  const category = filter.account_category;
  const status = filter.status;
  if (!needle && !category && status === null) {
    return { tree: nodes, expandedIds: [], matchedCount: 0 };
  }

  let matchedCount = 0;
  const walk = (list: AccountTreeNode[]): AccountTreeNode[] => {
    const kept: AccountTreeNode[] = [];
    for (const node of list) {
      const children = walk(node.children ?? []);
      const selfMatch =
        (!needle ||
          node.account_code.toLowerCase().includes(needle) ||
          node.account_name.toLowerCase().includes(needle)) &&
        (!category || node.account_category === category) &&
        (status === null || node.status === status);
      if (!selfMatch && children.length === 0) {
        continue;
      }
      if (selfMatch) {
        matchedCount += 1;
      }
      kept.push(children.length ? { ...node, children } : stripChildren(node));
    }
    return kept;
  };

  const tree = walk(nodes);
  const expandedIds = expandableAccountIds(tree);
  return { tree, expandedIds, matchedCount };
}

/** 复制节点但去掉 `children`（过滤后成了叶子，就不能再带空箭头） */
function stripChildren(node: AccountTreeNode): AccountTreeNode {
  const { children: _drop, ...rest } = node;
  return rest;
}

/** 「共 N 条科目 / M 个一级」这类副标题文案 */
export function accountTreeMeta(nodes: AccountTreeNode[]): string {
  const total = countAccounts(nodes);
  const roots = nodes.length;
  const depth = accountTreeDepth(nodes);
  if (total === 0) {
    return '暂无科目';
  }
  return `共 ${total} 条科目 · ${roots} 个一级 · 最深 ${depth} 级`;
}

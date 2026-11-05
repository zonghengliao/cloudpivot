/**
 * 条件规则树（`when` 子树）的纯逻辑层 —— 与后端 `business/flow/graph.rs::parse_condition` 逐字对齐。
 *
 * # 为什么单独一个 .ts
 *
 * 规则树的「生成 JSON / 反解 JSON / 上限判定 / 字段白名单」全在这里，**不 import Vue、不碰 DOM**，
 * 这样 `node --test`（Node 22 原生类型擦除）能直接跑单测，页面与单测共用同一份逻辑，杜绝漂移
 * —— 和 `board-receipt.ts` 的做法一致（本仓库没有 vitest，`pnpm test:unit` 就是 `node --test`）。
 *
 * # 与后端的契约（字段名一个字母都不能改）
 *
 * ```json
 * {"op":"cmp","field":"form.amount","cmp":"gte","value":10000}
 * {"op":"and","args":[{...},{...}]}
 * {"op":"or","args":[{...}]}
 * {"op":"not","args":[{...}]}
 * ```
 *
 * 上限（`graph.rs` 的常量，别在这里改小）：深度 ≤ 8、节点 ≤ 64、每组 args ≤ 16、
 * 字符串字面量 ≤ 200 字、数组字面量 ≤ 50 项；`form.*` 后缀 ≤ 64 个字符。
 */

/** 非 `form.*` 的固定字段白名单（逐字复制自 `graph::FIXED_FIELDS`，后端加字段时这里要同步）。 */
export const FIXED_FIELDS = [
  'initiator.id',
  'initiator.name',
  'initiator.dept_id',
  'instance.biz_type',
  'instance.biz_no',
  'instance.title',
  'task.status',
  'task.priority',
  'task.estimate_hours',
  'task.title',
] as const;

/** 字段下拉的中文提示（纯 UI 文案，不参与校验）。 */
export const FIXED_FIELD_LABELS: Record<string, string> = {
  'initiator.id': '发起人 ID',
  'initiator.name': '发起人姓名',
  'initiator.dept_id': '发起人部门 ID',
  'instance.biz_type': '业务类型',
  'instance.biz_no': '业务单号',
  'instance.title': '实例标题',
  'task.status': '任务状态',
  'task.priority': '任务优先级',
  'task.estimate_hours': '预估工时',
  'task.title': '任务标题',
};

// ==================== 后端常量（graph.rs） ====================

/** 条件嵌套层数上限（根节点算第 1 层）。 */
export const MAX_DEPTH = 8;
/** 条件节点总数上限（分组 + 比较节点一起数）。 */
export const MAX_NODES = 64;
/** 单个 and/or 的 args 上限。 */
export const MAX_ARGS = 16;
/** 字符串字面量字符数上限。 */
export const MAX_LITERAL_CHARS = 200;
/** 数组字面量项数上限（`in` / `nin`）。 */
export const MAX_LITERAL_ARRAY = 50;
/** `form.` 之后的字段名长度上限。 */
export const MAX_FORM_FIELD_CHARS = 64;

// ==================== 类型 ====================

/** 比较算子的字面量（后端 `CompareOp::parse` 认得 11 个）。 */
export type CompareOp =
  | 'eq'
  | 'ne'
  | 'gt'
  | 'gte'
  | 'lt'
  | 'lte'
  | 'in'
  | 'nin'
  | 'contains'
  | 'is_null'
  | 'not_null';

/** 逻辑算子。 */
export type GroupOp = 'and' | 'or' | 'not';

/** 条件里允许的字面量（后端 `check_literal`：无对象、无嵌套数组）。 */
export type ConditionLiteral =
  | string
  | number
  | boolean
  | null
  | Array<string | number | boolean | null>;

/** 单条件比较节点。 */
export interface CmpNode {
  op: 'cmp';
  field: string;
  cmp: CompareOp;
  /** `is_null` / `not_null` 不吃右值，这里固定 null 且不落盘。 */
  value: ConditionLiteral;
}

/** 逻辑分组节点。`not` 的 args 恒为 1 项。 */
export interface GroupNode {
  op: GroupOp;
  args: ConditionNode[];
}

/** 规则树节点。 */
export type ConditionNode = CmpNode | GroupNode;

/** 与后端交换的 JSON 形状（键名固定 op/field/cmp/value/args）。 */
export type ConditionJson = Record<string, unknown>;

/** 一条结构问题：挂在哪（path）＋ 中文原因。 */
export interface TreeIssue {
  /** 从根到该节点的下标路径，根为 `[]`；用于「红框」定位。 */
  path: number[];
  message: string;
}

/** 全树体检结果。 */
export interface TreeReport {
  issues: TreeIssue[];
  /** 节点总数（含分组）。 */
  nodes: number;
  /** 实际层数（空树记 0）。 */
  depth: number;
  ok: boolean;
}

// ==================== 算子表 ====================

/** 比较算子下拉（8 个常用在前，后端多支持的 3 个在后；顺序即下拉顺序）。 */
export const COMPARE_OPS: Array<{ value: CompareOp; label: string }> = [
  { value: 'eq', label: '= 等于' },
  { value: 'ne', label: '≠ 不等于' },
  { value: 'gt', label: '> 大于' },
  { value: 'gte', label: '≥ 大于等于' },
  { value: 'lt', label: '< 小于' },
  { value: 'lte', label: '≤ 小于等于' },
  { value: 'in', label: '属于（多值）' },
  { value: 'contains', label: '包含' },
  // 后端 CompareOp 也认这两个，留着是为了「打开老数据不丢算子」
  { value: 'nin', label: '不属于（多值）' },
  { value: 'is_null', label: '为空' },
  { value: 'not_null', label: '不为空' },
];

/** 分组算子下拉。 */
export const GROUP_OPS: Array<{ value: GroupOp; label: string }> = [
  { value: 'and', label: '且（and）' },
  { value: 'or', label: '或（or）' },
  { value: 'not', label: '非（not）' },
];

/** 该算子是否不吃右值（`is_null` / `not_null`）。 */
export function isUnaryOp(op: string): boolean {
  return op === 'is_null' || op === 'not_null';
}

/** 该算子是否要求右值是数组（`in` / `nin`）。 */
export function isArrayOp(op: string): boolean {
  return op === 'in' || op === 'nin';
}

// ==================== 字段白名单 ====================

/**
 * 字段合法性判定 —— 与后端 `graph::field_allowed` 同一套规则：
 * ① 逐字命中 10 个固定字段；② 或 `form.` + 合法标识符（首字符字母/下划线，其余字母数字下划线，≤64 字）。
 */
export function fieldAllowed(field: string): boolean {
  return fieldError(field) === null;
}

/** 字段不合法时返回中文原因，合法返回 null（供输入框「红框 + 提示」）。 */
export function fieldError(field: string): string | null {
  const trimmed = (field ?? '').trim();
  if (!trimmed) return '请先选择字段';
  if ((FIXED_FIELDS as readonly string[]).includes(trimmed)) return null;
  if (!trimmed.startsWith('form.')) {
    return `字段「${trimmed}」不在白名单内（只允许 form.* 或 10 个固定字段）`;
  }
  const rest = trimmed.slice('form.'.length);
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(rest)) {
    return 'form. 后面必须是表单字段名（字母或下划线开头，只含字母/数字/下划线）';
  }
  if (rest.length > MAX_FORM_FIELD_CHARS) {
    return `form. 后面的字段名最长 ${MAX_FORM_FIELD_CHARS} 个字符`;
  }
  return null;
}

// ==================== 字面量 ↔ 输入框文本 ====================

/**
 * 右值输入框文本 → JSON 字面量。
 *
 * - `in` / `nin`：按中英文逗号切分成数组（每项自动转数字/布尔/null，其余当字符串）
 * - 其它：`10000`→number、`true/false/null`→对应类型、其余→字符串
 *
 * 返回 `error` 时组件只标红、不提交。
 */
export function literalFromInput(
  text: string,
  op: string,
): { value: ConditionLiteral; error: string | null } {
  const raw = (text ?? '').trim();
  if (isUnaryOp(op)) return { value: null, error: null };
  if (!raw) return { value: '', error: '请填写右值' };

  const atom = (piece: string): string | number | boolean | null => {
    const text = piece.trim();
    if (text === 'true') return true;
    if (text === 'false') return false;
    if (text === 'null') return null;
    if (/^-?\d+(\.\d+)?$/.test(text)) return Number(text);
    return text;
  };

  if (isArrayOp(op)) {
    const items = raw
      .split(/[,，]/)
      .map((piece) => piece.trim())
      .filter((piece) => piece !== '')
      .map(atom);
    if (items.length === 0) return { value: [], error: '请至少填一个值（逗号分隔）' };
    if (items.length > MAX_LITERAL_ARRAY) {
      return { value: items, error: `数组字面量最多 ${MAX_LITERAL_ARRAY} 项` };
    }
    return { value: items, error: null };
  }

  const value = atom(raw);
  if (typeof value === 'string' && value.length > MAX_LITERAL_CHARS) {
    return { value, error: `字符串字面量最多 ${MAX_LITERAL_CHARS} 个字符` };
  }
  return { value, error: null };
}

/** JSON 字面量 → 输入框可编辑文本（`in` 用逗号拼）。 */
export function literalToInput(value: ConditionLiteral | undefined): string {
  if (value === null || value === undefined) return '';
  if (Array.isArray(value)) {
    return value.map((item) => (item === null ? 'null' : String(item))).join(', ');
  }
  return String(value);
}

// ==================== 节点构造 ====================

/** 造一个空的比较节点（字段留空 → 由体检标红提示）。 */
export function createCmpNode(): CmpNode {
  return { op: 'cmp', field: '', cmp: 'eq', value: '' };
}

/**
 * 造一个分组节点。
 *
 * 新分组默认带一个空比较节点：后端 `parse_condition` 拒收空 args，
 * 一次给一个子条件能让用户「点了就能填」，也让深度/节点数计算有个明确下界。
 */
export function createGroupNode(op: GroupOp = 'and'): GroupNode {
  return { op, args: [createCmpNode()] };
}

// ==================== 生成 JSON ====================

/**
 * 规则树 → JSON（键顺序固定 op → field → cmp → value / op → args，与规范文档示例逐字一致）。
 *
 * - `null` / 空树 → `null`（后端语义：无条件）
 * - `is_null` / `not_null` 不落 `value` 键（后端求值时置 null，写不写都接受，少写更干净）
 */
export function toJson(node: ConditionNode | null | undefined): ConditionJson | null {
  if (!node) return null;
  if (node.op === 'cmp') {
    const out: ConditionJson = { op: 'cmp', field: node.field, cmp: node.cmp };
    if (!isUnaryOp(node.cmp)) out.value = node.value;
    return out;
  }
  if (node.op === 'not') {
    // 后端要求恰好 1 项；编辑期允许暂缺（args 为空时占位 null，体检会标红）
    return { op: 'not', args: node.args.slice(0, 1).map((child) => toJson(child)) };
  }
  return { op: node.op, args: node.args.map((child) => toJson(child)) };
}

/** 树 → JSON 文本（预览/调试用）。 */
export function toJsonText(node: ConditionNode | null | undefined): string {
  const json = toJson(node);
  return json === null ? 'null' : JSON.stringify(json, null, 2);
}

// ==================== 反解 JSON ====================

const OPS = new Set(['and', 'or', 'not', 'cmp']);
const CMP_OPS = new Set<string>(COMPARE_OPS.map((op) => op.value));

/**
 * JSON → 规则树（打开已有节点时回填）。
 *
 * **不抛异常**：后端存进来的老数据可能带不认识的东西（比如早期版本的算子里），
 * 这时候把问题记进 `issues` 并尽量保住能渲染的部分 —— 编辑器不该因为一条脏数据白屏。
 */
export function fromJson(raw: unknown): { node: ConditionNode | null; issues: TreeIssue[] } {
  const issues: TreeIssue[] = [];
  if (raw === null || raw === undefined) return { node: null, issues };
  if (typeof raw !== 'object' || Array.isArray(raw)) {
    issues.push({ path: [], message: '条件必须是 JSON 对象（如 {"op":"cmp",…}）' });
    return { node: null, issues };
  }
  const node = fromJsonInner(raw as Record<string, unknown>, [], issues);
  return { node, issues };
}

function fromJsonInner(
  raw: Record<string, unknown>,
  path: number[],
  issues: TreeIssue[],
): ConditionNode | null {
  const opRaw = typeof raw.op === 'string' ? raw.op.trim().toLowerCase() : '';
  if (!opRaw) {
    issues.push({ path, message: '条件节点缺少 op 字段（and / or / not / cmp）' });
    return null;
  }
  if (!OPS.has(opRaw)) {
    issues.push({ path, message: `不支持的算子「${opRaw}」（只允许 and / or / not / cmp）` });
    return null;
  }

  if (opRaw === 'cmp') {
    const field = typeof raw.field === 'string' ? raw.field.trim() : '';
    const cmpRaw = typeof raw.cmp === 'string' ? raw.cmp.trim().toLowerCase() : '';
    const cmp = CMP_OPS.has(cmpRaw) ? (cmpRaw as CompareOp) : 'eq';
    if (!CMP_OPS.has(cmpRaw)) {
      issues.push({ path, message: `不支持的比较符「${cmpRaw || '空'}」` });
    }
    let value: ConditionLiteral = null;
    if (!isUnaryOp(cmp)) {
      const candidate = raw.value;
      if (Array.isArray(candidate)) {
        value = candidate.filter(
          (item): item is string | number | boolean | null =>
            item === null ||
            typeof item === 'string' ||
            typeof item === 'number' ||
            typeof item === 'boolean',
        );
      } else if (
        candidate === null ||
        typeof candidate === 'string' ||
        typeof candidate === 'number' ||
        typeof candidate === 'boolean'
      ) {
        value = candidate;
      } else if (candidate === undefined) {
        issues.push({ path, message: `cmp(${cmp}) 缺少 value` });
        value = '';
      } else {
        issues.push({ path, message: '右值只允许 string / number / bool / null / 数组' });
        value = '';
      }
    }
    return { op: 'cmp', field, cmp, value };
  }

  const argsRaw = raw.args;
  if (!Array.isArray(argsRaw)) {
    issues.push({
      path,
      message: opRaw === 'not' ? 'not 必须有 args 数组（且只放一个条件）' : `${opRaw} 必须有 args 数组`,
    });
    return { op: opRaw as GroupOp, args: [] };
  }
  const args: ConditionNode[] = [];
  argsRaw.forEach((item, index) => {
    if (item === null || typeof item !== 'object' || Array.isArray(item)) {
      issues.push({ path: [...path, index], message: '条件节点必须是 JSON 对象' });
      return;
    }
    const child = fromJsonInner(item as Record<string, unknown>, [...path, index], issues);
    if (child) args.push(child);
  });
  if (opRaw === 'not' && args.length > 1) {
    issues.push({ path, message: 'not 的 args 必须恰好 1 项（多余的已被忽略）' });
    return { op: 'not', args: args.slice(0, 1) };
  }
  return { op: opRaw as GroupOp, args };
}

// ==================== 结构体检（上限 + 字段白名单） ====================

/** 节点总数。 */
export function countNodes(node: ConditionNode | null | undefined): number {
  if (!node) return 0;
  if (node.op === 'cmp') return 1;
  return 1 + node.args.reduce((sum, child) => sum + countNodes(child), 0);
}

/** 实际层数（根 = 1；空树 = 0）。 */
export function treeDepth(node: ConditionNode | null | undefined): number {
  if (!node) return 0;
  if (node.op === 'cmp') return 1;
  if (node.args.length === 0) return 1;
  return 1 + Math.max(...node.args.map((child) => treeDepth(child)));
}

/** 全树体检：字段/算子/右值/args/深度/节点数。 */
export function validateTree(root: ConditionNode | null | undefined): TreeReport {
  const issues: TreeIssue[] = [];
  const walk = (node: ConditionNode, path: number[], depth: number): void => {
    if (depth > MAX_DEPTH) {
      issues.push({ path, message: `深度已达上限 ${MAX_DEPTH} 层` });
    }
    if (node.op === 'cmp') {
      const error = fieldError(node.field);
      if (error) issues.push({ path, message: error });
      if (!isUnaryOp(node.cmp)) {
        if (isArrayOp(node.cmp) && !Array.isArray(node.value)) {
          issues.push({ path, message: `${node.cmp} 的右值必须是数组` });
        } else if (
          typeof node.value === 'string' &&
          node.value.length > MAX_LITERAL_CHARS
        ) {
          issues.push({ path, message: `字符串字面量最多 ${MAX_LITERAL_CHARS} 个字符` });
        }
      }
      return;
    }
    if (node.op === 'not') {
      if (node.args.length !== 1) {
        issues.push({ path, message: 'not 必须恰好有 1 个子条件' });
      }
    } else {
      if (node.args.length === 0) {
        issues.push({ path, message: `${node.op} 至少要 1 个条件` });
      }
      if (node.args.length > MAX_ARGS) {
        issues.push({ path, message: `同一分组最多 ${MAX_ARGS} 个条件` });
      }
    }
    node.args.forEach((child, index) => walk(child, [...path, index], depth + 1));
  };

  const nodes = countNodes(root);
  const depth = treeDepth(root);
  if (root) walk(root, [], 1);
  if (nodes > MAX_NODES) {
    issues.push({ path: [], message: `条件节点数已达上限 ${MAX_NODES} 个（当前 ${nodes}）` });
  }
  return { issues, nodes, depth, ok: issues.length === 0 };
}

/** 把体检结果转成 `path.join('.') → 第一条原因` 的索引，组件按行标红用。 */
export function issueIndex(report: TreeReport): Record<string, string> {
  const map: Record<string, string> = {};
  for (const issue of report.issues) {
    const key = pathKey(issue.path);
    if (!(key in map)) map[key] = issue.message;
  }
  return map;
}

/** 路径 → 稳定字符串键（根是 `''`）。 */
export function pathKey(path: number[]): string {
  return path.join('.');
}

// ==================== 增删（就地修改，供响应式树使用） ====================

/** 按路径取节点。 */
export function nodeAtPath(
  root: ConditionNode | null | undefined,
  path: number[],
): ConditionNode | null {
  let cursor: ConditionNode | null = root ?? null;
  for (const index of path) {
    if (!cursor || cursor.op === 'cmp') return null;
    cursor = cursor.args[index] ?? null;
  }
  return cursor;
}

/** 按路径取父分组 + 下标。 */
export function parentOfPath(
  root: ConditionNode | null | undefined,
  path: number[],
): { parent: GroupNode; index: number } | null {
  if (path.length === 0) return null;
  const parent = nodeAtPath(root, path.slice(0, -1));
  if (!parent || parent.op === 'cmp') return null;
  return { parent, index: path[path.length - 1] as number };
}

/** 这一组还能不能再加子节点：`null` 表示可以，字符串是不可以的原因。 */
export interface AddBlock {
  /** 加一个比较条件被挡住的原因 */
  cmp: string | null;
  /** 加一个分组被挡住的原因 */
  group: string | null;
}

/**
 * 判定「在某分组下加子节点」是否越界（§3.4 三项上限 + not 只许 1 项）。
 *
 * `group` 会连带一个默认的比较子节点，所以按 **2 层** 深、**2 个** 节点算。
 */
export function addBlock(root: ConditionNode | null | undefined, groupPath: number[]): AddBlock {
  const group = nodeAtPath(root, groupPath);
  if (!group || group.op === 'cmp') {
    return { cmp: '这里不能加子条件', group: '这里不能加子条件' };
  }
  if (group.op === 'not') {
    const reason = '「非」只能有 1 个子条件';
    return { cmp: reason, group: reason };
  }
  const depth = groupPath.length + 1; // 该分组自身的层数（根 = 1）
  const nodes = countNodes(root);
  const argsFull = group.args.length >= MAX_ARGS ? `同一分组最多 ${MAX_ARGS} 个条件` : null;

  const depthBlock = (need: number): string | null =>
    depth + need > MAX_DEPTH ? `深度已达上限 ${MAX_DEPTH} 层` : null;
  const nodeBlock = (need: number): string | null =>
    nodes + need > MAX_NODES ? `条件节点数已达上限 ${MAX_NODES} 个` : null;

  return {
    cmp: argsFull ?? depthBlock(1) ?? nodeBlock(1),
    group: argsFull ?? depthBlock(2) ?? nodeBlock(2),
  };
}

/** 往某分组末尾加子节点，返回新节点的路径；被上限挡住返回 null。 */
export function addChild(
  root: ConditionNode | null | undefined,
  groupPath: number[],
  kind: 'cmp' | 'group',
  groupOp: GroupOp = 'and',
): number[] | null {
  const group = nodeAtPath(root, groupPath);
  if (!group || group.op === 'cmp') return null;
  const block = addBlock(root, groupPath);
  if ((kind === 'cmp' ? block.cmp : block.group) !== null) return null;
  const child = kind === 'cmp' ? createCmpNode() : createGroupNode(groupOp);
  group.args.push(child);
  return [...groupPath, group.args.length - 1];
}

/** 删掉某节点；返回删完之后的整棵树（根被删则返回 null）。 */
export function removeAt(
  root: ConditionNode | null | undefined,
  path: number[],
): ConditionNode | null {
  if (!root) return null;
  if (path.length === 0) return null;
  const hit = parentOfPath(root, path);
  if (!hit) return root;
  hit.parent.args.splice(hit.index, 1);
  return root;
}

// ==================== 杂项 ====================

/** 语义相等比较（键顺序无关）——用于判断 watch 进来的 modelValue 是否就是自己刚抛出去的那份。 */
export function semanticKey(value: unknown): string {
  const sort = (input: unknown): unknown => {
    if (Array.isArray(input)) return input.map(sort);
    if (input && typeof input === 'object') {
      const out: Record<string, unknown> = {};
      for (const key of Object.keys(input as Record<string, unknown>).sort()) {
        out[key] = sort((input as Record<string, unknown>)[key]);
      }
      return out;
    }
    return input;
  };
  try {
    return JSON.stringify(sort(value)) ?? 'null';
  } catch {
    return 'null';
  }
}

/**
 * 字段下拉选项（内置 10 个固定字段；`creatable` 让用户直接敲 `form.xxx`）。
 *
 * 标签口径是**中文名在前**（`发起人姓名（initiator.name）`），不是键名在前：
 * 面板可拖到 280px，键名在前时 select 会截成「initiator.nam…」—— 用户看到的全是术语，
 * 中文名恰好被省略号吃掉（实测 320px 就是这样）。键名仍留在括号里，给高级用户核对条件表达式。
 */
export function fieldOptions(): Array<{ value: string; label: string }> {
  return FIXED_FIELDS.map((field) => ({
    value: field,
    label: `${FIXED_FIELD_LABELS[field] ?? field}（${field}）`,
  }));
}

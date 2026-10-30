/**
 * 规则预览（P1-B3 · ② 人话翻译）的纯逻辑层。
 *
 * 把「WHEN 条件树 + THEN 目标」翻译成一两句中文：
 *
 * ```
 * 当 金额 ≥ 10000 且 部门 是 财务 时 → 由 财务总监 审批
 * ```
 *
 * # 三条硬要求（卡面）
 *
 * 1. **嵌套用括号表达优先级**：`当 (A 且 B) 或 C 时`。
 *    同算子的嵌套直接摊平（`and(a, and(b, c))` 与 `and(a, b, c)` 在 DSL 里等价，摊平不丢信息，
 *    少一层括号更好读）；**不同算子**、以及 `not` 的子节点，一律加括号，保证读者不会读错优先级。
 * 2. 空规则树 → 「尚未配置规则」。
 * 3. 非法节点 → 「⚠ 第 N 条规则有空字段」。
 *
 * # 不 import Vue
 *
 * 与 `condition-tree.ts` / `then-target.ts` / `rule-validation.ts` 同一套做法：
 * `pnpm test:unit`（`tsx --test`）直接跑，预览文案的每个字都被单测钉住。
 */
import {
  FIXED_FIELD_LABELS,
  fieldAllowed,
  fromJson,
  isUnaryOp,
  type ConditionNode,
} from '@/views/business/flow/components/condition-tree';
import type { ThenTarget } from '@/views/business/flow/components/then-target';
import { createTarget, fromThenJson } from '@/views/business/flow/components/then-target';
import {
  validateRule,
  walkTree,
  type RuleReport,
} from '@/views/business/flow/components/rule-validation';

/** 比较算子的中文读法（`gte` 用 `≥`、`eq` 用「是」，与卡面样例一致）。 */
export const OP_WORDS: Record<string, string> = {
  eq: '是',
  ne: '不是',
  gt: '>',
  gte: '≥',
  lt: '<',
  lte: '≤',
  in: '属于',
  nin: '不属于',
  contains: '包含',
  is_null: '为空',
  not_null: '不为空',
};

/** 分组算子的连接词。 */
export const GROUP_WORDS: Record<'and' | 'or', string> = {
  and: '且',
  or: '或',
};

/** then 目标的读法（`then-target.ts` 的 `describeTarget` 偏「编辑器口吻」，这里要「人话」）。 */
const TARGET_WORDS: Record<string, string> = {
  INITIATOR: '发起人',
  TASK_OWNER: '任务负责人',
  TASK_CREATOR: '任务创建人',
  TASK_ASSIGNER: '任务派发人',
  ADMIN: '平台管理员',
};

/** 预览入参。 */
export interface PreviewInput {
  /** 后端 `edge.condition` 原文 */
  condition: unknown;
  /** then 目标（`undefined` = 不展示 THEN 段） */
  targets?: readonly ThenTarget[] | undefined;
  /** 自定义字段中文名（`form.amount → 金额`）；缺省用 `condition-tree.ts` 的内置表，再缺省显示原名 */
  fieldLabels?: Record<string, string> | undefined;
  /** 外部已经算过的体检结果（避免重复计算） */
  report?: RuleReport | undefined;
  /** 规则 id（后端 `id`，缺省 `r_1`） */
  ruleId?: string | undefined;
}

/** 预览结果。 */
export interface RulePreviewModel {
  /** 条件树为空（无条件 = 总是通过） */
  empty: boolean;
  /** 分行展示：`['当 … 时', '→ 由 … 审批']`（空树只有一行「尚未配置规则」） */
  lines: string[];
  /** 单行全文 */
  sentence: string;
  /** `⚠ …` 提示（空数组 = 没有需要提醒的地方） */
  warnings: string[];
  /** 体检结果（面板直接复用） */
  report: RuleReport;
}

/** 空树的固定文案。 */
export const EMPTY_TEXT = '尚未配置规则';

/**
 * 把规则翻译成人话。
 *
 * 返回的 `lines` 里**不含**计数行（`⚠ 结构：8/8 层`）—— 计数是面板的 `n/limit` 渲染，
 * 由 `rule-validation.ts::countText` 提供，这里只负责「句子」和「提醒」。
 */
export function translateRule(input: PreviewInput): RulePreviewModel {
  const report = input.report ?? validateRule({ condition: input.condition, targets: input.targets });
  const parsed = fromJson(input.condition);
  const tree = parsed.node;

  if (!tree) {
    return {
      empty: true,
      lines: [EMPTY_TEXT],
      sentence: EMPTY_TEXT,
      warnings: collectWarnings(report, input.ruleId),
      report,
    };
  }

  const when = conditionSentence(tree, input.fieldLabels);
  const lines = [`当 ${when} 时`];
  if (input.targets !== undefined) {
    lines.push(`→ 由 ${thenPhrase(input.targets, input.fieldLabels)} 审批`);
  }
  return {
    empty: false,
    lines,
    sentence: lines.join(' '),
    warnings: collectWarnings(report, input.ruleId),
    report,
  };
}

/**
 * 条件树 → 一句话（不含「当 … 时」外壳）。
 *
 * 括号规则：子节点是**不同算子**的分组、或是 `not` 时加 `()`；同算子分组摊平。
 */
export function conditionSentence(
  node: ConditionNode,
  fieldLabels?: Record<string, string> | undefined,
): string {
  if (node.op === 'cmp') return cmpSentence(node, fieldLabels);
  if (node.op === 'not') {
    const inner = node.args[0];
    if (!inner) return '非(（空）)';
    return `非(${conditionSentence(inner, fieldLabels)})`;
  }
  if (node.args.length === 0) return '（空分组）';
  const words = node.args.map((child) => {
    if (child.op === 'cmp') return cmpSentence(child, fieldLabels);
    if (child.op === 'not') return conditionSentence(child, fieldLabels);
    // 同算子摊平：and(a, and(b, c)) === and(a, b, c)
    if (child.op === node.op) return flatten(child as LogicGroup, fieldLabels);
    return `(${conditionSentence(child, fieldLabels)})`;
  });
  return words.join(` ${GROUP_WORDS[node.op]} `);
}

/** `and` / `or` 分组（`not` 之外的逻辑分组，摊平与加括号都以它为准）。 */
type LogicGroup = { op: 'and' | 'or'; args: ConditionNode[] };

/** 同算子分组的摊平（只拼子句，不加括号）。 */
function flatten(
  group: LogicGroup,
  fieldLabels?: Record<string, string> | undefined,
): string {
  return group.args
    .map((child) => {
      if (child.op === 'cmp') return cmpSentence(child, fieldLabels);
      if (child.op === 'not') return conditionSentence(child, fieldLabels);
      if (child.op === group.op) return flatten(child as LogicGroup, fieldLabels);
      return `(${conditionSentence(child, fieldLabels)})`;
    })
    .join(` ${GROUP_WORDS[group.op]} `);
}

/** 单条比较 → `金额 ≥ 10000`。 */
export function cmpSentence(
  node: Extract<ConditionNode, { op: 'cmp' }>,
  fieldLabels?: Record<string, string> | undefined,
): string {
  const field = (node.field ?? '').trim();
  const label = fieldLabel(field, fieldLabels);
  const word = OP_WORDS[node.cmp] ?? node.cmp;
  if (isUnaryOp(node.cmp)) return `${label} ${word}`;
  return `${label} ${word} ${literalText(node.value)}`;
}

/** 字段中文名：调用方给的 → 内置表 → 原名。 */
export function fieldLabel(field: string, fieldLabels?: Record<string, string> | undefined): string {
  const trimmed = (field ?? '').trim();
  if (!trimmed) return '（未选字段）';
  if (fieldLabels && trimmed in fieldLabels) return fieldLabels[trimmed] as string;
  if (trimmed in FIXED_FIELD_LABELS) return FIXED_FIELD_LABELS[trimmed] as string;
  return trimmed;
}

/** 右值 → 文本（字符串不带引号，与卡面样例「部门 是 财务」一致）。 */
export function literalText(value: unknown): string {
  if (value === null || value === undefined || value === '') return '（空）';
  if (Array.isArray(value)) {
    if (value.length === 0) return '（空列表）';
    return value.map((item) => String(item)).join('、');
  }
  return String(value);
}

/**
 * THEN 目标 → 一句话（不含「由 … 审批」外壳）。
 *
 * 多个目标用「、」连：`由 角色 FINANCE_DIRECTOR、发起人 审批`。
 */
export function thenPhrase(
  targets: readonly ThenTarget[] | undefined,
  fieldLabels?: Record<string, string> | undefined,
): string {
  if (!targets || targets.length === 0) return '（未配置审批人）';
  void fieldLabels;
  return targets.map((target) => targetPhrase(target)).join('、');
}

/** 单个目标的人话读法。 */
export function targetPhrase(target: ThenTarget): string {
  const type = (target.type ?? '').trim();
  if (!type) return '（未选来源）';
  const ids = (list: readonly string[] | undefined): string[] =>
    (list ?? []).map((item) => item.trim()).filter((item) => item !== '');

  switch (type) {
    case 'USER': {
      const users = ids(target.userIds);
      return users.length === 0 ? '指定用户（未选人）' : `指定用户（${users.length} 人）`;
    }
    case 'ROLE': {
      const codes = ids(target.roleCodes);
      return codes.length === 0 ? '按角色（未选）' : `角色 ${codes.join('、')}`;
    }
    case 'DEPT_LEADER': {
      const depts = ids(target.deptIds);
      return depts.length === 0 ? '发起人部门负责人' : `部门负责人（${depts.length} 个部门）`;
    }
    case 'INITIATOR_SUPERIOR':
      return `发起人上级（上溯 ${Number(target.levels) || 1} 级）`;
    case 'FORM_VAR':
      return `表单字段「${(target.formVarKey ?? '').trim() || '（未填）'}」指向的人`;
    default:
      return TARGET_WORDS[type] ?? `（不支持的来源 ${type}）`;
  }
}

/**
 * 体检结果 → `⚠ …` 提醒。
 *
 * 卡面点名的两种：`⚠ 第 N 条规则有空字段`（字段没选）与计数行（面板另渲染）。
 * 其余原因照后端原文跟在「第 N 条规则」后面，不再造新词。
 */
export function collectWarnings(report: RuleReport, ruleId?: string | undefined): string[] {
  void ruleId;
  const warnings: string[] = [];
  const push = (text: string): void => {
    if (!warnings.includes(text)) warnings.push(text);
  };
  for (const issue of report.issues) {
    if (issue.kind === 'target') {
      push(`⚠ ${issue.reason}`);
      continue;
    }
    if (issue.kind === 'structure' && issue.ruleIndex === null && issue.path.length === 0) {
      // 整树问题（深度 / 节点 / 规则数）交给面板的计数行，不重复列一遍
      continue;
    }
    if (issue.ruleIndex === null) continue;
    const trimmedField = issue.reason.includes('缺少 field');
    push(trimmedField ? `⚠ 第 ${issue.ruleIndex} 条规则有空字段` : `⚠ 第 ${issue.ruleIndex} 条规则：${issue.reason}`);
  }
  return warnings;
}

/** 条件树里的每个叶子是否都填了字段（面板 `空字段 N 处` 用）。 */
export function emptyFieldCount(condition: unknown): number {
  const tree = fromJson(condition).node;
  if (!tree) return 0;
  let count = 0;
  walkTree(tree, [], (node) => {
    if (node.op !== 'cmp') return;
    const field = (node.field ?? '').trim();
    if (!field || !fieldAllowed(field)) count += 1;
  });
  return count;
}

// ==================== 目标节点 → then 目标（预览 / 校验共用） ====================

/** 审批人行（`FlowAssignee` 的结构子集，只要用到两个字段）。 */
export interface AssigneeRowLike {
  type: string;
  ref_id: string;
}

/** 目标节点的结构子集（`FlowNode` 满足它，单测可以少造字段）。 */
export interface TargetNodeLike {
  assignee_type?: string | null;
  superior_levels?: number;
  form_var_key?: string | null;
  config?: unknown;
  assignees?: readonly AssigneeRowLike[] | undefined;
}

/**
 * 目标节点 → `then` 目标数组（预览与校验的唯一入口）。
 *
 * - `undefined`：没给节点（不展示 / 不校验 THEN 段）；
 * - `[]`：节点存在但**没配审批人** → 预览显示「（未配置审批人）」、校验给 65016；
 * - `EXPRESSION`：真值在 `node.config.expression.rules[0].then`，交给 `fromThenJson` 反解；
 * - 其余来源：`assignee_type` + `assignees`（`ROLE` 的 `ref_id` 是角色 id，
 *   而 DSL 要的是 `role_codes` —— 预览这里只做人话展示，不做「id→编码」的翻译，
 *   因为那需要再查一次角色表；校验目标数 / 空目标不受影响）。
 */
export function thenTargetsFromNode(node: TargetNodeLike | null | undefined): ThenTarget[] | undefined {
  if (!node) return undefined;
  const type = (node.assignee_type ?? '').trim();
  if (!type) return [];

  const rows = (node.assignees ?? []).filter((row) => (row.ref_id ?? '').trim() !== '');
  const refs = (kind: string): string[] =>
    rows.filter((row) => row.type === kind).map((row) => row.ref_id.trim());

  if (type === 'EXPRESSION') {
    const expression = extractExpression(node.config);
    const rules = expression?.rules;
    if (!Array.isArray(rules) || rules.length === 0) return [];
    const first = rules[0] as { then?: unknown };
    return fromThenJson(first?.then).targets;
  }

  const target = createTarget(type);
  switch (type) {
    case 'USER':
      return [{ ...target, userIds: refs('USER') }];
    case 'ROLE':
      return [{ ...target, roleCodes: refs('ROLE') }];
    case 'DEPT_LEADER':
      return [{ ...target, deptIds: refs('DEPT_LEADER') }];
    case 'INITIATOR_SUPERIOR':
      return [{ ...target, levels: Number(node.superior_levels) || 1 }];
    case 'FORM_VAR':
      return [{ ...target, formVarKey: (node.form_var_key ?? '').trim() }];
    default:
      return [target];
  }
}

/** 从 `node.config` 里取 `expression` 正文（缺 / 形状不对 → null）。 */
function extractExpression(config: unknown): { rules?: unknown } | null {
  if (!config || typeof config !== 'object' || Array.isArray(config)) return null;
  const expression = (config as Record<string, unknown>).expression;
  if (!expression || typeof expression !== 'object' || Array.isArray(expression)) return null;
  return expression as { rules?: unknown };
}

/**
 * 实时校验（P1-B3 · ① 结构上限 / ② 字段白名单 / ③ 错误码文案对齐）的纯逻辑层。
 *
 * # 为什么不是塞进组件里
 *
 * 三条闸（深度 8 / 节点 64 / 规则 32）、白名单、then 目标校验全部在这里，
 * **不 import Vue、不碰 DOM**，所以 `pnpm test:unit`（`tsx --test`）能直接跑，
 * 页面与单测共用同一份判定 —— 与 `condition-tree.ts`（P1-B1）/`then-target.ts`（P1-B2）同一套做法。
 *
 * # 与后端的口径（见 `flow-error-codes.ts` 头注释的分工表）
 *
 * - WHEN 条件树的**结构 / 上限 / 字段白名单** → 后端 `graph.rs::parse_condition` 拒 → **65016**，
 *   全文是 `边 {key}（{from} → {to}）的条件表达式非法：{reason}`；
 * - then 目标（空 / 超 5 个 / 来源不支持）→ 落在**同一个 65016**（`节点 {key}（{name}）的审批人表达式不合法：{reason}`）；
 * - 只有 `PREV_APPROVER` 是 **65033**。
 *
 * 每条 issue 带 `aligned`：`true` = `message` 与后端 `msg` **逐字一致**（单测逐个断言）；
 * `false` = 纯前端编辑期提示（例如「规则数 ≤ 32」这条后端没有的闸、脏数据的形状提示），
 * 这类 issue **不带 code**，不会让用户误以为后端会返回那个码。
 */
import {
  MAX_ARGS,
  countNodes,
  fieldAllowed,
  fromJson,
  isArrayOp,
  isUnaryOp,
  treeDepth,
  type ConditionNode,
} from '@/views/business/flow/components/condition-tree';
import {
  MAX_TARGETS,
  isReserved,
  isTargetType,
  targetIssues,
  type ThenTarget,
} from '@/views/business/flow/components/then-target';
import {
  FLOW_CONDITION_INVALID,
  FLOW_EXPRESSION_INVALID,
  MAX_CONDITION_ARGS,
  MAX_CONDITION_DEPTH,
  MAX_CONDITION_NODES,
  MAX_LITERAL_ARRAY,
  MAX_LITERAL_CHARS,
  MAX_RULES,
  fill,
  RUST_TEMPLATES,
  edgeConditionMessage,
  nodeExpressionMessage,
  prevApproverMessage,
  reasonArgs,
  reasonArgsEmpty,
  reasonArrayValue,
  reasonDepth,
  reasonField,
  reasonFieldMissing,
  reasonLiteralArray,
  reasonLiteralChars,
  reasonNodes,
  reasonNotArgs,
  reasonTargetEmpty,
  reasonTargetsOver,
} from '@/views/business/flow/components/flow-error-codes';

/** 问题分类（与卡面 ①②③ 一一对应）。 */
export type RuleIssueKind = 'shape' | 'structure' | 'whitelist' | 'target';

/** 一条实时校验问题。 */
export interface RuleIssue {
  /** 分类：形状 / 结构上限 / 字段白名单 / then 目标 */
  kind: RuleIssueKind;
  /** 后端会返回的码（`65016` / `65033`）；纯前端闸与脏数据提示为 `null` */
  code: number | null;
  /** 用户看到的一句话（`aligned` 时与后端 msg 逐字一致） */
  message: string;
  /** 未包外壳的原因（`aligned` 时 = 后端 `format!` 出来那一段） */
  reason: string;
  /** 树内定位路径（根为 `[]`）；then 目标问题为 `[]` */
  path: number[];
  /** 第几条规则（叶子序号，1 起；分组/整树问题为 `null`） */
  ruleIndex: number | null;
  /** `message` 是否与后端文案逐字一致 */
  aligned: boolean;
}

/** 面板上的计数器。 */
export interface RuleCounts {
  /** 实际层数（根 = 1；空树 = 0） */
  depth: number;
  /** 节点总数（分组 + 比较） */
  nodes: number;
  /** 规则（分支）数 = 叶子比较节点数 */
  rules: number;
  /** then 目标个数 */
  targets: number;
}

/** 各项上限（面板显示 `n/limit` 用）。 */
export interface RuleLimits {
  depth: number;
  nodes: number;
  rules: number;
  targets: number;
}

/** 上限快照。 */
export const RULE_LIMITS: RuleLimits = {
  depth: MAX_CONDITION_DEPTH,
  nodes: MAX_CONDITION_NODES,
  rules: MAX_RULES,
  targets: MAX_TARGETS,
};

/** 校验入参。 */
export interface RuleInput {
  /** 后端 `edge.condition` 原文（`null` = 无条件） */
  condition: unknown;
  /** then 目标（`undefined` = 该节点不是表达式审批人，不校验目标） */
  targets?: readonly ThenTarget[] | undefined;
  /** 边信息：给了就出「边 e1（n1 → n2）…」的完整后端文案 */
  edge?: { key: string; from: string; to: string } | null | undefined;
  /** 节点信息：给了就出「节点 n3（财务总监）…」的完整后端文案 */
  node?: { key: string; name: string } | null | undefined;
  /** 规则 id（后端 `expression` 里的 `id`，缺省 `r_1`） */
  ruleId?: string | undefined;
}

/** 整份体检结果。 */
export interface RuleReport {
  issues: RuleIssue[];
  counts: RuleCounts;
  limits: RuleLimits;
  /** 全部通过（空树也算通过：无条件 = 总是通过） */
  ok: boolean;
  /** 出现过的后端码（去重，升序） */
  codes: number[];
}

/** 空树（无条件）的计数。 */
const EMPTY_COUNTS: RuleCounts = { depth: 0, nodes: 0, rules: 0, targets: 0 };

/** 叶子（比较节点）数 —— 即「规则 / 分支」数。 */
export function countRules(node: ConditionNode | null | undefined): number {
  if (!node) return 0;
  if (node.op === 'cmp') return 1;
  return node.args.reduce((sum, child) => sum + countRules(child), 0);
}

/**
 * 体检一份规则（WHEN 条件树 + THEN 目标）。
 *
 * 判序：形状 → 结构上限 → 逐节点（args / 白名单 / 右值）→ then 目标。
 * 一次返回**全部**问题，不做「报第一条就停」—— 用户一次就能看全要改的地方。
 */
export function validateRule(input: RuleInput): RuleReport {
  const issues: RuleIssue[] = [];
  const push = (issue: RuleIssue): void => {
    issues.push(issue);
  };

  // ---------- ① 形状（脏数据）：纯前端提示，不给码 ----------
  const parsed = fromJson(input.condition);
  for (const shapeIssue of parsed.issues) {
    push({
      kind: 'shape',
      code: null,
      message: shapeIssue.message,
      reason: shapeIssue.message,
      path: shapeIssue.path,
      ruleIndex: null,
      aligned: false,
    });
  }

  const tree = parsed.node;
  const targets = input.targets;
  const counts: RuleCounts = tree
    ? {
        depth: treeDepth(tree),
        nodes: countNodes(tree),
        rules: countRules(tree),
        targets: targets?.length ?? 0,
      }
    : { ...EMPTY_COUNTS, targets: targets?.length ?? 0 };

  // 边的完整文案外壳（没给边就退化成单句原因，仍是后端原文）
  const edgeMessage = (reason: string): string =>
    input.edge ? edgeConditionMessage(input.edge, reason) : reason;

  // ---------- ② 结构上限 ----------
  if (tree) {
    if (counts.depth > MAX_CONDITION_DEPTH) {
      const reason = reasonDepth();
      push({
        kind: 'structure',
        code: FLOW_CONDITION_INVALID,
        message: edgeMessage(reason),
        reason,
        path: [],
        ruleIndex: null,
        aligned: true,
      });
    }
    if (counts.nodes > MAX_CONDITION_NODES) {
      const reason = reasonNodes();
      push({
        kind: 'structure',
        code: FLOW_CONDITION_INVALID,
        message: edgeMessage(reason),
        reason,
        path: [],
        ruleIndex: null,
        aligned: true,
      });
    }
    if (counts.rules > MAX_RULES) {
      // 后端没有这条（见 flow-error-codes.ts 的 MAX_RULES 注释），所以不挂码
      const reason = `规则（分支）数不能超过 ${MAX_RULES} 条（当前 ${counts.rules} 条）`;
      push({
        kind: 'structure',
        code: null,
        message: reason,
        reason,
        path: [],
        ruleIndex: null,
        aligned: false,
      });
    }

    // ---------- ③ 逐节点：args / 白名单 / 右值 ----------
    walkTree(tree, [], (node, path, ruleIndex) => {
      if (node.op === 'cmp') {
        checkCmp(node.field, node.cmp, node.value, path, ruleIndex);
        return;
      }
      if (node.op === 'not') {
        if (node.args.length !== 1) {
          const reason = reasonNotArgs();
          push({
            kind: 'structure',
            code: FLOW_CONDITION_INVALID,
            message: edgeMessage(reason),
            reason,
            path,
            ruleIndex: null,
            aligned: true,
          });
        }
        return;
      }
      if (node.args.length === 0) {
        const reason = reasonArgsEmpty(node.op);
        push({
          kind: 'structure',
          code: FLOW_CONDITION_INVALID,
          message: edgeMessage(reason),
          reason,
          path,
          ruleIndex: null,
          aligned: true,
        });
      }
      if (node.args.length > MAX_CONDITION_ARGS) {
        const reason = reasonArgs(node.op);
        push({
          kind: 'structure',
          code: FLOW_CONDITION_INVALID,
          message: edgeMessage(reason),
          reason,
          path,
          ruleIndex: null,
          aligned: true,
        });
      }
    });
  }

  /** 单条比较节点的判定（字段白名单 + 右值形状 / 长度）。 */
  function checkCmp(
    field: string,
    cmp: string,
    value: unknown,
    path: number[],
    ruleIndex: number | null,
  ): void {
    const trimmed = (field ?? '').trim();
    if (!trimmed) {
      const reason = reasonFieldMissing();
      push({
        kind: 'whitelist',
        code: FLOW_CONDITION_INVALID,
        message: edgeMessage(reason),
        reason,
        path,
        ruleIndex,
        aligned: true,
      });
    } else if (!fieldAllowed(trimmed)) {
      const reason = reasonField(trimmed);
      push({
        kind: 'whitelist',
        code: FLOW_CONDITION_INVALID,
        message: edgeMessage(reason),
        reason,
        path,
        ruleIndex,
        aligned: true,
      });
    }

    if (isUnaryOp(cmp)) return;
    if (isArrayOp(cmp)) {
      if (!Array.isArray(value)) {
        const reason = reasonArrayValue(cmp);
        push({
          kind: 'structure',
          code: FLOW_CONDITION_INVALID,
          message: edgeMessage(reason),
          reason,
          path,
          ruleIndex,
          aligned: true,
        });
      } else if (value.length > MAX_LITERAL_ARRAY) {
        const reason = reasonLiteralArray();
        push({
          kind: 'structure',
          code: FLOW_CONDITION_INVALID,
          message: edgeMessage(reason),
          reason,
          path,
          ruleIndex,
          aligned: true,
        });
      }
      return;
    }
    if (typeof value === 'string' && value.length > MAX_LITERAL_CHARS) {
      const reason = reasonLiteralChars();
      push({
        kind: 'structure',
        code: FLOW_CONDITION_INVALID,
        message: edgeMessage(reason),
        reason,
        path,
        ruleIndex,
        aligned: true,
      });
    }
  }

  // ---------- ④ then 目标 ----------
  if (targets) {
    for (const issue of validateThenTargets(targets, input)) push(issue);
  }

  const codes = [...new Set(issues.map((issue) => issue.code).filter((code): code is number => code !== null))].sort(
    (left, right) => left - right,
  );

  return {
    issues,
    counts,
    limits: RULE_LIMITS,
    ok: issues.length === 0,
    codes,
  };
}

/**
 * then 目标校验（对齐 `expression.rs::parse_targets` + `parse_target`）。
 *
 * 空 / 超 5 个 / 来源不支持 → `65016`（后端也是 65016，不是卡面写的 65033）；
 * `PREV_APPROVER` → `65033`。
 */
export function validateThenTargets(
  targets: readonly ThenTarget[],
  input: Pick<RuleInput, 'node' | 'ruleId'> = {},
): RuleIssue[] {
  const issues: RuleIssue[] = [];
  const whereDesc = whereDescription(input.ruleId);
  const wrap = (reason: string): string =>
    input.node ? nodeExpressionMessage(input.node, reason) : reason;

  if (targets.length === 0) {
    const reason = reasonTargetEmpty(whereDesc);
    issues.push({
      kind: 'target',
      code: FLOW_CONDITION_INVALID,
      message: wrap(reason),
      reason,
      path: [],
      ruleIndex: null,
      aligned: true,
    });
    return issues;
  }

  if (targets.length > MAX_TARGETS) {
    const reason = reasonTargetsOver(whereDesc, targets.length);
    issues.push({
      kind: 'target',
      code: FLOW_CONDITION_INVALID,
      message: wrap(reason),
      reason,
      path: [],
      ruleIndex: null,
      aligned: true,
    });
  }

  targets.forEach((target, index) => {
    // 保留词：本期唯一会拿到 65033 的场景（D-E8）
    if (isReserved(target.type.trim())) {
      const reason = fill(RUST_TEMPLATES.prevApproverRuntime, {});
      issues.push({
        kind: 'target',
        code: FLOW_EXPRESSION_INVALID,
        message: input.node ? prevApproverMessage(input.node) : reason,
        reason,
        path: [],
        ruleIndex: index + 1,
        aligned: true,
      });
      return;
    }
    if (!isTargetType(target.type.trim())) {
      // 来源不认识：后端 `parse_target` 以「来源不支持」400（65016 外壳）
      const reason = `第 ${index + 1} 个审批人的来源「${target.type.trim() || '空'}」不支持`;
      issues.push({
        kind: 'target',
        code: FLOW_CONDITION_INVALID,
        message: wrap(reason),
        reason,
        path: [],
        ruleIndex: index + 1,
        aligned: false,
      });
      return;
    }
    // 具体字段问题（选人 / 角色编码 / 上溯层数 …）：编辑期提示，后端的是同名不同句，不冒充对齐
    for (const detail of targetIssues(target)) {
      issues.push({
        kind: 'target',
        code: FLOW_CONDITION_INVALID,
        message: detail,
        reason: detail,
        path: [],
        ruleIndex: index + 1,
        aligned: false,
      });
    }
  });

  return issues;
}

/** 后端 `where_desc` 的形如「第 1 条规则（r_1）」。 */
export function whereDescription(ruleId?: string | undefined): string {
  return `第 1 条规则（${(ruleId ?? '').trim() || 'r_1'}）`;
}

/**
 * 遍历规则树，给每个节点回调 `(node, path, ruleIndex)`。
 *
 * `ruleIndex` 是**叶子（比较节点）的序号**，1 起，按先序计数 ——
 * 预览区「第 N 条规则有空字段」用的就是它，用户数下来的顺序与下拉里看到的一致。
 */
export function walkTree(
  node: ConditionNode,
  path: number[],
  visit: (node: ConditionNode, path: number[], ruleIndex: number | null) => void,
  counter: { value: number } = { value: 0 },
): void {
  if (node.op === 'cmp') {
    counter.value += 1;
    visit(node, path, counter.value);
    return;
  }
  visit(node, path, null);
  node.args.forEach((child, index) => walkTree(child, [...path, index], visit, counter));
}

/** `path → 第一条问题`（按行红框用；根路径是 `''`）。 */
export function issueByPath(report: RuleReport): Record<string, string> {
  const map: Record<string, string> = {};
  for (const issue of report.issues) {
    const key = issue.path.join('.');
    if (!(key in map)) map[key] = issue.message;
  }
  return map;
}

/** `ruleIndex → 第一条问题`（预览区「第 N 条规则…」用）。 */
export function issueByRule(report: RuleReport): Record<number, string> {
  const map: Record<number, string> = {};
  for (const issue of report.issues) {
    if (issue.ruleIndex === null || issue.ruleIndex <= 0) continue;
    if (!(issue.ruleIndex in map)) map[issue.ruleIndex] = issue.message;
  }
  return map;
}

/** 计数器文本：`8/8 层`、`23/64`、`3/32 条`（面板直接用）。 */
export function countText(report: RuleReport): { depth: string; nodes: string; rules: string } {
  return {
    depth: `${report.counts.depth}/${report.limits.depth} 层`,
    nodes: `${report.counts.nodes}/${report.limits.nodes}`,
    rules: `${report.counts.rules}/${report.limits.rules} 条`,
  };
}

/** 是否超限（面板决定红不红）。 */
export function isOverLimit(report: RuleReport): { depth: boolean; nodes: boolean; rules: boolean } {
  return {
    depth: report.counts.depth > report.limits.depth,
    nodes: report.counts.nodes > report.limits.nodes,
    rules: report.counts.rules > report.limits.rules,
  };
}

/** 供外部复用的常量（`MAX_ARGS` 等由 condition-tree 提供，这里只做一处转发，避免各处各写一个数）。 */
export const CONDITION_MAX_ARGS = MAX_ARGS;

/**
 * `rule-validation.ts` + `flow-error-codes.ts` 单测 —— 实时校验三条闸 + 错误码文案对齐。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`）���
 *
 * # 这份测试的两个作用
 *
 * 1. **行为**：深度 8/9、节点 64/65、规则 32/33、白名单、then 目标 —— 边界两侧各一条，
 *    改常量而没改判定时立刻红。
 * 2. **对齐**：直接读 `backend/src/**` 的 rust 源码，断言前端常量 / 文案模板与后端**逐字一致**。
 *    这是卡面 ⑤ 的落地方式 —— 不靠人眼比对，靠 `fs.readFileSync`。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import {
  CONDITION_MAX_ARGS,
  RULE_LIMITS,
  countRules,
  countText,
  issueByPath,
  isOverLimit,
  validateRule,
  validateThenTargets,
  whereDescription,
  type RuleReport,
} from '../../src/views/business/flow/components/rule-validation.ts';
import {
  ALIGNED_CODES,
  FLOW_CONDITION_INVALID,
  FLOW_EXPRESSION_INVALID,
  MAX_CONDITION_ARGS,
  MAX_CONDITION_DEPTH,
  MAX_CONDITION_NODES,
  MAX_EXPR_RULES,
  MAX_EXPR_TARGETS,
  MAX_RULES,
  RUST_TEMPLATES,
  TEMPLATE_SOURCES,
  edgeConditionMessage,
  fill,
  nodeExpressionMessage,
  prevApproverMessage,
  reasonDepth,
  reasonField,
  reasonNodes,
} from '../../src/views/business/flow/components/flow-error-codes.ts';
import { FIXED_FIELDS } from '../../src/views/business/flow/components/condition-tree.ts';
import { createTarget, type ThenTarget } from '../../src/views/business/flow/components/then-target.ts';

// ==================== 造数工具 ====================

/** 后端 JSON 形状的比较节点 */
const cmp = (field: string, op = 'gte', value: unknown = 10000): Record<string, unknown> => ({
  op: 'cmp',
  field,
  cmp: op,
  value,
});

/** and / or 分组 */
const group = (op: 'and' | 'or', args: unknown[]): Record<string, unknown> => ({ op, args });

/** not 分组（只放一个） */
const not = (arg: unknown): Record<string, unknown> => ({ op: 'not', args: [arg] });

/** 一条边（后端文案外壳用） */
const EDGE = { key: 'e1', from: 'n1', to: 'n2' };
/** 一个节点（审批人表达式外壳用） */
const NODE = { key: 'n3', name: '财务总监审批' };

/** then 目标快捷造法 */
const roleTarget = (code = 'FINANCE_DIRECTOR'): ThenTarget => ({
  ...createTarget('ROLE'),
  roleCodes: [code],
});

/** 造一棵「层级为 depth 的 not 链」（最内层是 cmp） */
function notChain(depth: number): Record<string, unknown> {
  let node: Record<string, unknown> = cmp('form.amount');
  for (let level = 1; level < depth; level += 1) node = not(node);
  return node;
}

/** 造一棵平衡分组树（每层 2 个子节点，args ≤ 16、depth ≤ 7），共 2*leaves-1 个节点 */
function balanced(leaves: number): Record<string, unknown> {
  if (leaves === 1) return cmp('form.amount');
  const left = Math.ceil(leaves / 2);
  return group('and', [balanced(left), balanced(leaves - left)]);
}

/**
 * 造 `total` 个节点的树（节点数边界用）。
 *
 * `balanced` 的节点数恒为奇数（2*leaves-1），偶数要额外塞一个「只有 1 个子的分组」；
 * 这样 64 个节点时叶子正好 32 个（同时踩住「规则 ≤ 32」那条闸），
 * 且每层 args ≤ 2、depth ≤ 7 —— **不顺手触发 args / depth 上限**。
 */
function nodesTree(total: number): Record<string, unknown> {
  if (total % 2 === 1) return balanced((total + 1) / 2);
  const leaves = total / 2; // 32 → 63 个节点
  const tree = balanced(leaves) as { op: string; args: unknown[] };
  // 把第一个叶子换成一个单子分组：+1 个节点，叶子数不变
  const wrap = (node: unknown): unknown => {
    const candidate = node as { op: string; args?: unknown[] };
    if (candidate.op === 'cmp') return group('and', [node]);
    const args = candidate.args as unknown[];
    return { op: candidate.op, args: args.map((child, index) => (index === 0 ? wrap(child) : child)) };
  };
  return wrap(tree) as Record<string, unknown>;
}

/** 叶子全合法的报告（断言用） */
function codesOf(report: RuleReport): number[] {
  return report.codes;
}

// ==================== ① 结构上限：深度 ====================

test('深度 8 层 → 通过（后端 MAX_CONDITION_DEPTH 边界值本身合法）', () => {
  const report = validateRule({ condition: notChain(8), edge: EDGE, targets: [roleTarget()] });
  assert.equal(report.counts.depth, 8);
  assert.equal(report.counts.nodes, 8);
  assert.equal(report.ok, true, JSON.stringify(report.issues));
  assert.deepEqual(codesOf(report), []);
});

test('深度 9 层 → 报错，且文案是 65016 + 后端原文', () => {
  const report = validateRule({ condition: notChain(9), edge: EDGE, targets: [roleTarget()] });
  assert.equal(report.counts.depth, 9);
  assert.equal(report.ok, false);
  const hit = report.issues.find((issue) => issue.kind === 'structure');
  assert.ok(hit, '应有结构问题');
  assert.equal(hit.code, FLOW_CONDITION_INVALID);
  assert.equal(hit.aligned, true);
  assert.equal(
    hit.message,
    '边 e1（n1 → n2）的条件表达式非法：条件嵌套层数超过 8 层（请把条件拆成几条边）',
  );
  assert.deepEqual(codesOf(report), [65016]);
});

// ==================== ① 结构上限：节点数 ====================

test('节点 64 个 → 通过（32 个叶子 + 32 个分组）', () => {
  const tree = nodesTree(64);
  const report = validateRule({ condition: tree, edge: EDGE, targets: [roleTarget()] });
  assert.equal(report.counts.nodes, 64);
  assert.equal(report.counts.rules, 32);
  assert.equal(report.ok, true, JSON.stringify(report.issues));
});

test('节点 65 个 → 报错（65016 节点数上限原文）', () => {
  const report = validateRule({ condition: nodesTree(65), edge: EDGE, targets: [roleTarget()] });
  assert.equal(report.counts.nodes, 65);
  assert.equal(report.ok, false);
  const hit = report.issues.find((issue) => issue.reason === reasonNodes());
  assert.ok(hit, '应命中节点数上限');
  assert.equal(hit.code, FLOW_CONDITION_INVALID);
  assert.equal(hit.aligned, true);
});

// ==================== ① 结构上限：规则（分支）数 ====================

test('规则 32 条 → 通过', () => {
  // 两个 and 各 16 个叶子（args 上限也是 16，别顺手踩到）
  const tree = group('and', [
    group('and', Array.from({ length: 16 }, (_, index) => cmp(`form.r${index}`))),
    group('and', Array.from({ length: 16 }, (_, index) => cmp(`form.s${index}`))),
  ]);
  const report = validateRule({ condition: tree, edge: EDGE, targets: [roleTarget()] });
  assert.equal(report.counts.rules, 32);
  assert.equal(report.ok, true, JSON.stringify(report.issues));
});

test('规则 33 条 → 报错，且是纯前端闸（不挂后端码）', () => {
  const tree = group('and', [
    group('and', Array.from({ length: 16 }, (_, index) => cmp(`form.r${index}`))),
    group('or', Array.from({ length: 16 }, (_, index) => cmp(`form.s${index}`))),
    cmp('form.tail'),
  ]);
  const report = validateRule({ condition: tree, edge: EDGE, targets: [roleTarget()] });
  assert.equal(report.counts.rules, 33);
  assert.equal(report.ok, false);
  const hit = report.issues.find((issue) => issue.kind === 'structure' && issue.path.length === 0);
  assert.ok(hit, '应命中规则数上限');
  assert.equal(hit.code, null, '规则 32 是前端闸，后端没有这个码');
  assert.equal(hit.aligned, false);
  assert.equal(hit.message, '规则（分支）数不能超过 32 条（当前 33 条）');
});

// ==================== ② 字段白名单 ====================

test('10 个固定字段 + form.* → 通过', () => {
  const tree = group('and', [
    ...FIXED_FIELDS.map((field) => cmp(field)),
    cmp('form.amount'),
    cmp('form.dept_name', 'eq', '财务'),
  ]);
  const report = validateRule({ condition: tree, edge: EDGE, targets: [roleTarget()] });
  assert.equal(report.ok, true, JSON.stringify(report.issues));
  assert.equal(report.counts.rules, 12);
});

test('非白名单字段 → 报错，文案与 graph.rs 逐字一致', () => {
  const report = validateRule({ condition: cmp('user.nickname'), edge: EDGE, targets: [roleTarget()] });
  assert.equal(report.ok, false);
  const hit = report.issues[0];
  assert.ok(hit);
  assert.equal(hit.kind, 'whitelist');
  assert.equal(hit.code, FLOW_CONDITION_INVALID);
  assert.equal(hit.ruleIndex, 1);
  assert.equal(
    hit.reason,
    '字段 user.nickname 不在白名单内（允许 form.*、initiator.id/name/dept_id、instance.biz_type/biz_no/title、task.status/priority/estimate_hours）',
  );
  assert.equal(hit.message, edgeConditionMessage(EDGE, hit.reason));
  assert.equal(hit.aligned, true);
});

test('空字段 → 「cmp 节点缺少 field」，并定位到第 1 条规则', () => {
  const report = validateRule({ condition: group('and', [cmp(''), cmp('form.amount')]), edge: EDGE });
  const hit = report.issues[0];
  assert.ok(hit);
  assert.equal(hit.reason, 'cmp 节点缺少 field');
  assert.equal(hit.ruleIndex, 1);
  assert.deepEqual(hit.path, [0]);
  const byRule = issueByPath(report);
  assert.ok(byRule['0'], '红框要能按 path 定位到第 1 个叶子');
});

// ==================== ③ then 目标 ====================

test('then 目标为空 → 65016 + expression.rs 原文（不是卡面写的 65033）', () => {
  const issues = validateThenTargets([], { node: NODE });
  assert.equal(issues.length, 1);
  const hit = issues[0];
  assert.ok(hit);
  assert.equal(hit.code, FLOW_CONDITION_INVALID);
  assert.equal(hit.aligned, true);
  assert.equal(
    hit.message,
    '节点 n3（财务总监审批）的审批人表达式不合法：第 1 条规则（r_1）的审批人不能为空（至少 1 个）',
  );
});

test('then 目标 6 个（上限 5）→ 65016 超限原文', () => {
  const targets = Array.from({ length: 6 }, (_, index) => roleTarget(`ROLE_${index}`));
  const report = validateRule({ condition: cmp('form.amount'), targets, node: NODE });
  const hit = report.issues.find((issue) => issue.reason.includes('审批人数超过'));
  assert.ok(hit, JSON.stringify(report.issues));
  assert.equal(hit.code, FLOW_CONDITION_INVALID);
  assert.equal(hit.reason, `${whereDescription()}的审批人数超过 5 个（当前 6 个）`);
});

test('PREV_APPROVER → 65033 + graph.rs 原文（本期唯一给 65033 的场景）', () => {
  const targets: ThenTarget[] = [{ ...createTarget('USER'), type: 'PREV_APPROVER' }];
  const issues = validateThenTargets(targets, { node: NODE });
  const hit = issues.find((issue) => issue.code === FLOW_EXPRESSION_INVALID);
  assert.ok(hit, JSON.stringify(issues));
  assert.equal(hit.aligned, true);
  assert.equal(
    hit.message,
    '节点 n3（财务总监审批）的审批人表达式用了「上一节点的审批人」（PREV_APPROVER），本版本未开放（P2），请先改用其它来源',
  );
  assert.equal(hit.message, prevApproverMessage(NODE));
});

// ==================== 对齐：读后端 rust 源码断言 ====================

/** 读后端源码（路径相对本测试文件） */
function rust(relative: string): string {
  return readFileSync(
    fileURLToPath(new URL(`../../../backend/src/business/flow/${relative}`, import.meta.url)),
    'utf8',
  );
}

/** 读 error_code.rs */
function errorCodeSource(): string {
  return readFileSync(
    fileURLToPath(new URL('../../../backend/src/shared/error_code.rs', import.meta.url)),
    'utf8',
  );
}

/** 从 rust 源码里抠一个 `pub const NAME: type = 值;` */
function rustConst(source: string, name: string): string {
  const matched = new RegExp(`pub const ${name}:\\s*\\w+\\s*=\\s*([^;]+);`).exec(source);
  assert.ok(matched, `后端源码里找不到常量 ${name}`);
  return (matched[1] as string).trim();
}

test('码值 65016 / 65033 与 error_code.rs 一致', () => {
  const source = errorCodeSource();
  assert.equal(FLOW_CONDITION_INVALID, Number(rustConst(source, 'FLOW_CONDITION_INVALID')));
  assert.equal(FLOW_EXPRESSION_INVALID, Number(rustConst(source, 'FLOW_EXPRESSION_INVALID')));
  assert.deepEqual([...ALIGNED_CODES], [65016, 65033]);
});

test('上限常量与 graph.rs / expression.rs 一致', () => {
  const graph = rust('graph.rs');
  const expression = rust('expression.rs');
  assert.equal(MAX_CONDITION_DEPTH, Number(rustConst(graph, 'MAX_CONDITION_DEPTH')));
  assert.equal(MAX_CONDITION_NODES, Number(rustConst(graph, 'MAX_CONDITION_NODES')));
  assert.equal(MAX_CONDITION_ARGS, Number(rustConst(graph, 'MAX_CONDITION_ARGS')));
  assert.equal(MAX_EXPR_RULES, Number(rustConst(expression, 'MAX_EXPR_RULES')));
  assert.equal(MAX_EXPR_TARGETS, Number(rustConst(expression, 'MAX_EXPR_TARGETS')));
  // 前端自己那套常量也得跟这套对齐（防两处各写一个数）
  assert.equal(CONDITION_MAX_ARGS, MAX_CONDITION_ARGS);
  assert.deepEqual(RULE_LIMITS, { depth: 8, nodes: 64, rules: MAX_RULES, targets: 5 });
});

test('FIXED_FIELDS 与 graph.rs 逐项一致（顺序也算）', () => {
  const graph = rust('graph.rs');
  const matched = /pub const FIXED_FIELDS:\s*\[&str;\s*(\d+)\]\s*=\s*\[([\s\S]*?)\];/.exec(graph);
  assert.ok(matched, 'graph.rs 里没找到 FIXED_FIELDS');
  const backendFields = [...(matched[2] as string).matchAll(/"([^"]+)"/g)].map((item) => item[1]);
  assert.equal(Number(matched[1]), backendFields.length);
  assert.equal(backendFields.length, 10);
  assert.deepEqual([...FIXED_FIELDS], backendFields);
});

test('65016 / 65033 的文案模板在对应 .rs 里逐字存在', () => {
  const sources: Record<string, string> = {};
  for (const [name, files] of Object.entries(TEMPLATE_SOURCES)) {
    const template = RUST_TEMPLATES[name as keyof typeof RUST_TEMPLATES];
    for (const file of files) {
      sources[file] ??= rust(file);
      assert.ok(
        sources[file].includes(`"${template}"`),
        `${file} 里找不到逐字模板（${name}）：\n  期望： "${template}"`,
      );
    }
  }
  assert.equal(Object.keys(TEMPLATE_SOURCES).length, Object.keys(RUST_TEMPLATES).length);
});

test('填模板的结果与 rust format! 的语义一致（占位按序 / 具名都能填）', () => {
  assert.equal(reasonDepth(), '条件嵌套层数超过 8 层（请把条件拆成几条边）');
  assert.equal(reasonNodes(), '条件表达式节点数超过 64 个（请把条件拆成几条边）');
  assert.equal(reasonField('task.foo'), `字段 task.foo 不在白名单内（允许 form.*、initiator.id/name/dept_id、instance.biz_type/biz_no/title、task.status/priority/estimate_hours）`);
  assert.equal(
    fill(RUST_TEMPLATES.reasonArgs, { MAX_CONDITION_ARGS, op: 'or' }, []),
    'or 的 args 最多 16 项',
  );
  assert.equal(
    fill(RUST_TEMPLATES.reasonRulesOver, { MAX_EXPR_RULES }, [17]),
    '规则数超过 16 条（当前 17 条）',
  );
  assert.equal(
    nodeExpressionMessage(NODE, reasonDepth()),
    '节点 n3（财务总监审批）的审批人表达式不合法：条件嵌套层数超过 8 层（请把条件拆成几条边）',
  );
  // 占位缺失要抛错（等同 rust 的编译期错误）
  assert.throws(() => fill(RUST_TEMPLATES.edgeCondition, {}, ['e1']));
});

// ==================== 杂项 ====================

test('空条件树 = 无条件：合法，计数归零', () => {
  const report = validateRule({ condition: null, edge: EDGE });
  assert.equal(report.ok, true);
  assert.deepEqual(report.counts, { depth: 0, nodes: 0, rules: 0, targets: 0 });
  assert.equal(report.codes.length, 0);
});

test('计数器与超限标记（面板 n/limit 渲染用）', () => {
  const ok = validateRule({ condition: group('and', [cmp('form.a'), cmp('form.b')]), targets: [roleTarget()] });
  assert.deepEqual(countText(ok), { depth: '2/8 层', nodes: '3/64', rules: '2/32 条' });
  assert.deepEqual(isOverLimit(ok), { depth: false, nodes: false, rules: false });
  assert.equal(countRules(null), 0);

  const bad = validateRule({
    condition: group('and', Array.from({ length: 33 }, (_, index) => cmp(`form.r${index}`))),
  });
  assert.deepEqual(isOverLimit(bad), { depth: false, nodes: false, rules: true });
});

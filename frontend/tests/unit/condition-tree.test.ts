/**
 * `condition-tree.ts` 单测 —— when 条件树编辑器的生成/反解/上限/白名单契约。
 *
 * 运行：`pnpm test:unit`（= `node --test tests/unit/**\/*.test.ts`，Node 22 原生类型擦除，零新依赖）。
 *
 * 为什么测的是 .ts 而不是 .vue：规则树的全部判定都在 `condition-tree.ts` 里，组件只负责渲染；
 * 生成的 JSON 必须与后端 `business/flow/graph.rs::parse_condition` 逐字兼容，
 * 所以这里把「键名 / 键顺序 / 上限数值」都写死成断言，后端改常量时这里会先红。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  MAX_ARGS,
  MAX_DEPTH,
  MAX_NODES,
  addBlock,
  addChild,
  createCmpNode,
  createGroupNode,
  fieldError,
  fromJson,
  literalFromInput,
  nodeAtPath,
  removeAt,
  semanticKey,
  toJson,
  toJsonText,
  treeDepth,
  countNodes,
  validateTree,
  type CmpNode,
  type ConditionNode,
  type GroupNode,
} from '../../src/views/business/flow/components/condition-tree.ts';

/** 造一个填好的比较节点 */
const cmp = (field: string, op: CmpNode['cmp'], value: CmpNode['value']): CmpNode => ({
  op: 'cmp',
  field,
  cmp: op,
  value,
});

/** 造一个分组 */
const group = (op: GroupNode['op'], ...args: ConditionNode[]): GroupNode => ({ op, args });

/** 往返一次（JSON → 树 → JSON） */
const roundTrip = (json: unknown): unknown => toJson(fromJson(json).node);

// ==================== ① 单个 cmp ====================

test('① 单个 cmp 节点生成正确 JSON（键名/键顺序 = op field cmp value）', () => {
  const json = toJson(cmp('form.amount', 'gte', 10000));
  assert.deepEqual(json, { op: 'cmp', field: 'form.amount', cmp: 'gte', value: 10000 });
  // 键顺序也要稳定：直接比对序列化文本，和后端示例逐字一致
  assert.equal(JSON.stringify(json), '{"op":"cmp","field":"form.amount","cmp":"gte","value":10000}');
  // 字符串右值原样保留
  assert.deepEqual(toJson(cmp('instance.biz_type', 'eq', 'TASK')), {
    op: 'cmp',
    field: 'instance.biz_type',
    cmp: 'eq',
    value: 'TASK',
  });
});

// ==================== ② and 两项 ====================

test('② and 包两个条件 → args 长度 2，结构正确', () => {
  const tree = group('and', cmp('form.amount', 'gte', 10000), cmp('instance.biz_type', 'eq', 'TASK'));
  const json = toJson(tree) as { op: string; args: unknown[] };
  assert.equal(json.op, 'and');
  assert.equal(json.args.length, 2);
  assert.deepEqual(json.args[0], {
    op: 'cmp',
    field: 'form.amount',
    cmp: 'gte',
    value: 10000,
  });
  assert.deepEqual(json.args[1], {
    op: 'cmp',
    field: 'instance.biz_type',
    cmp: 'eq',
    value: 'TASK',
  });
});

// ==================== ③ or 套 and ====================

test('③ or 嵌套 and → 树形结构正确（or.args[0] 是 and，and.args 有 2 项）', () => {
  const tree = group(
    'or',
    group('and', cmp('form.amount', 'gt', 5000), cmp('task.priority', 'lte', 2)),
    cmp('initiator.dept_id', 'eq', 42),
  );
  const json = toJson(tree) as { op: string; args: Array<Record<string, unknown>> };
  assert.equal(json.op, 'or');
  assert.equal(json.args.length, 2);
  const nested = json.args[0] as { op: string; args: unknown[] };
  assert.equal(nested.op, 'and');
  assert.equal(nested.args.length, 2);
  assert.deepEqual(json.args[1], {
    op: 'cmp',
    field: 'initiator.dept_id',
    cmp: 'eq',
    value: 42,
  });
  // 层数：or(1) → and(2) → cmp(3)
  assert.equal(treeDepth(tree), 3);
  assert.equal(countNodes(tree), 5);
});

// ==================== ④ not 单条件 ====================

test('④ not 单条件 → args 长度 1（多给会被截断到 1 并记问题）', () => {
  const json = toJson(group('not', cmp('task.status', 'eq', 'DONE'))) as {
    op: string;
    args: unknown[];
  };
  assert.equal(json.op, 'not');
  assert.equal(json.args.length, 1);
  assert.deepEqual(json.args[0], { op: 'cmp', field: 'task.status', cmp: 'eq', value: 'DONE' });

  // 反解时 not 带 2 项：保住第一个 + 记一条问题（后端会拒，编辑器要能提示）
  const parsed = fromJson({ op: 'not', args: [{ op: 'cmp', field: 'task.title', cmp: 'eq', value: 'a' }, { op: 'cmp', field: 'task.title', cmp: 'eq', value: 'b' }] });
  assert.equal((parsed.node as GroupNode).args.length, 1);
  assert.equal(parsed.issues.length, 1);
  assert.match(parsed.issues[0]?.message ?? '', /not 的 args 必须恰好 1 项/);
});

// ==================== ⑤ 往返一致 ====================

test('⑤ JSON → 树 → JSON 逐字相等（往返一致）', () => {
  const samples: unknown[] = [
    { op: 'cmp', field: 'form.amount', cmp: 'gte', value: 10000 },
    {
      op: 'and',
      args: [
        { op: 'cmp', field: 'form.amount', cmp: 'gte', value: 10000 },
        { op: 'cmp', field: 'instance.biz_type', cmp: 'eq', value: 'TASK' },
      ],
    },
    {
      op: 'or',
      args: [
        {
          op: 'and',
          args: [
            { op: 'cmp', field: 'task.priority', cmp: 'lte', value: 2 },
            { op: 'cmp', field: 'form.tag', cmp: 'in', value: ['urgent', 'vip'] },
          ],
        },
        { op: 'not', args: [{ op: 'cmp', field: 'task.status', cmp: 'eq', value: 'DONE' }] },
      ],
    },
  ];
  for (const sample of samples) {
    const text = JSON.stringify(sample);
    const back = roundTrip(sample);
    assert.equal(JSON.stringify(back), text, `往返后必须逐字相同：${text}`);
    // 语义键也相同（键顺序无关的比较）
    assert.equal(semanticKey(back), semanticKey(sample));
  }
  // 反解不能报问题
  assert.equal(fromJson(samples[2]).issues.length, 0);
});

// ==================== ⑥ 深度到 8 ====================

test('⑥ 深度到 8 层 → 不能再加（addBlock 给出原因，addChild 拒绝）', () => {
  // 造一条 8 层的链：and ×7 → cmp（层数 = 7 个分组 + 1 个比较节点）
  let node: ConditionNode = cmp('form.x', 'eq', 1);
  for (let rest = MAX_DEPTH - 1; rest >= 1; rest -= 1) node = group('and', node);
  assert.equal(treeDepth(node), MAX_DEPTH, '构造出来的树应正好 8 层');
  assert.equal(countNodes(node), MAX_DEPTH);

  // 最深的分组在第 7 层（路径长度 6）；它的子条件正好落到第 8 层
  const deepestGroupPath = Array.from({ length: MAX_DEPTH - 2 }, () => 0);
  const deepest = nodeAtPath(node, deepestGroupPath);
  assert.ok(deepest && deepest.op === 'and', '这一层应该是分组');
  assert.equal(deepestGroupPath.length + 1, MAX_DEPTH - 1, '它在第 7 层');

  const block = addBlock(node, deepestGroupPath);
  // 「添加分组」要多占一层（新分组自身）＋ 组内默认条件再占一层 → 9 层 > 8，必须挡住
  assert.equal(block.group, `深度已达上限 ${MAX_DEPTH} 层`);
  assert.equal(addChild(node, deepestGroupPath, 'group'), null, '按钮此时是 disabled 的');
  // 「添加条件」只多占一层 → 刚好落在第 8 层，仍然允许（再往里就没地方了）
  assert.equal(block.cmp, null);
  assert.equal(treeDepth(node), MAX_DEPTH);
  assert.equal(validateTree(node).ok, true, '8 层本身合法（超的只是「再加」）');

  // 一旦第 7 层塞满子条件，第 8 层就只能是比较节点 —— 深度提示走红
  const atLimit = nodeAtPath(node, [...deepestGroupPath, 0]);
  assert.ok(atLimit && atLimit.op === 'cmp');
});

test('⑥b 手工构造「第 8 层还是分组」→ 两个添加按钮都被深度挡住', () => {
  // 正常编辑到不了（新分组自带子条件，会先撞上限），但反解来的老数据可能长这样
  const root: GroupNode = group('and');
  let cursor = root;
  const path: number[] = [];
  for (let level = 2; level <= MAX_DEPTH; level += 1) {
    const child: GroupNode = group('and');
    cursor.args.push(child);
    cursor = child;
    path.push(0);
  }
  // path 现在指向第 8 层的那个空分组
  assert.equal(path.length + 1, MAX_DEPTH);
  assert.equal(treeDepth(root), MAX_DEPTH);

  const block = addBlock(root, path);
  assert.equal(block.cmp, `深度已达上限 ${MAX_DEPTH} 层`);
  assert.equal(block.group, `深度已达上限 ${MAX_DEPTH} 层`);
  assert.equal(addChild(root, path, 'cmp'), null);
  assert.equal(addChild(root, path, 'group'), null);
});


// ==================== ⑦ 节点到 64 ====================

test('⑦ 节点到 64 → 不能再加；65 个节点被体检测出超限', () => {
  /** 造「根 and + 若干 and 子组，每组 n 个 cmp」的合法宽树（每组 args ≤ 16，绕开 args 上限先撞节点上限） */
  const wide = (...sizes: number[]): GroupNode => ({
    op: 'and',
    args: sizes.map((size, groupIndex) => ({
      op: 'and',
      args: Array.from({ length: size }, (_, index) => cmp('form.f', 'eq', groupIndex * 100 + index)),
    })),
  });

  // 根(1) + 4 个分组 + 59 个比较节点 = 64，刚好到顶（每组 ≤ 15 项 < 16，合法）
  const full = wide(15, 15, 15, 14);
  assert.equal(countNodes(full), MAX_NODES);
  assert.equal(validateTree(full).ok, true, '64 个节点本身合法（只是不能再加）');
  // 第 4 个子组（路径 [3]，14 项）想再加条件 → 被节点上限挡住
  assert.equal(addBlock(full, [3]).cmp, `条件节点数已达上限 ${MAX_NODES} 个`);
  assert.equal(addChild(full, [3], 'cmp'), null, '按钮此时是 disabled 的');
  // 根上加分组更不可能（+2 个节点）
  assert.equal(addBlock(full, []).group, `条件节点数已达上限 ${MAX_NODES} 个`);
  assert.equal(addChild(full, [], 'group'), null);

  // 63 个节点时还能再加一个（addChild 生效，正好补齐到 64）
  const near = wide(15, 15, 15, 13);
  assert.equal(countNodes(near), MAX_NODES - 1);
  assert.equal(addBlock(near, [3]).cmp, null);
  assert.deepEqual(addChild(near, [3], 'cmp'), [3, 13]);
  assert.equal(countNodes(near), MAX_NODES);

  // 65 个节点（越界数据）→ 体检报「节点数已达上限」
  const over = wide(15, 15, 15, 15);
  const report = validateTree(over);
  assert.equal(report.nodes, MAX_NODES + 1);
  assert.equal(report.ok, false);
  assert.ok(report.issues.some((issue) => /节点数已达上限 64/.test(issue.message)));
});

// ==================== ⑧ 字段白名单 ====================

test('⑧ 字段不在白名单（foo.bar）→ 红框提示；空字段也提示', () => {
  const bad = fieldError('foo.bar');
  assert.ok(bad && /不在白名单内/.test(bad), 'foo.bar 必须被拦下');

  // 组件用同一条错误做「红框 + 行内红字」
  const tree = cmp('foo.bar', 'eq', 1);
  const report = validateTree(tree);
  assert.equal(report.ok, false);
  assert.equal(report.issues[0]?.message, bad);

  assert.ok(fieldError('') !== null, '空字段要提示先选字段');
  assert.ok(fieldError('form.a-b') !== null, 'form. 后缀含连字符不合法');
  assert.ok(fieldError('Form.amount') !== null, '前缀大小写敏感（后端 strip_prefix 只认小写）');
  assert.ok(fieldError('instances.biz_type') !== null, '固定字段必须逐字命中');
  // 20 个固定字段里挑两个确认放行
  assert.equal(fieldError('initiator.dept_id'), null);
  assert.equal(fieldError('task.estimate_hours'), null);
});

// ==================== ⑨ 空树 = null ====================

test('⑨ 空规则树 → 生成 null（表示无条件）', () => {
  assert.equal(toJson(null), null);
  assert.equal(toJson(undefined), null);
  assert.equal(toJsonText(null), 'null');
  assert.equal(fromJson(null).node, null);
  assert.equal(fromJson(undefined).node, null);
  // 反解 null 不该报问题（这就是「无条件」的合法表示）
  assert.equal(fromJson(null).issues.length, 0);
  // 从树里把根删掉 → 回到 null
  const single = cmp('form.amount', 'gte', 1);
  assert.equal(removeAt(single, []), null);
});

// ==================== ⑩ form. 前缀 ====================

test('⑩ form.amount 前缀输入 → 通过校验，且生成 JSON 带完整字段名', () => {
  assert.equal(fieldError('form.amount'), null);
  assert.equal(fieldError('form._private_1'), null, '下划线开头也合法（后端允许）');
  assert.equal(validateTree(cmp('form.amount', 'gte', 10000)).ok, true);
  assert.deepEqual(toJson(cmp('form.amount', 'gte', 10000)), {
    op: 'cmp',
    field: 'form.amount',
    cmp: 'gte',
    value: 10000,
  });
  // 后端上限：form. 后缀最长 64 个字符
  assert.equal(fieldError(`form.${'a'.repeat(64)}`), null);
  assert.ok(fieldError(`form.${'a'.repeat(65)}`) !== null);
});

// ==================== 附加：右值输入 / 算子形状 / args 上限 ====================

test('附加：右值输入框文本 → JSON 字面量（数字/布尔/null/数组）', () => {
  assert.deepEqual(literalFromInput('10000', 'gte'), { value: 10000, error: null });
  assert.deepEqual(literalFromInput('TASK', 'eq'), { value: 'TASK', error: null });
  assert.deepEqual(literalFromInput('true', 'eq'), { value: true, error: null });
  assert.deepEqual(literalFromInput('null', 'eq'), { value: null, error: null });
  assert.deepEqual(literalFromInput('100, 200 ，vip', 'in'), {
    value: [100, 200, 'vip'],
    error: null,
  });
  assert.equal(literalFromInput('', 'eq').error, '请填写右值');
  // is_null / not_null 不吃右值
  assert.deepEqual(literalFromInput('随便填', 'is_null'), { value: null, error: null });
});

test('附加：is_null / not_null 不落 value 键（与后端求值形状一致）', () => {
  assert.deepEqual(toJson(cmp('task.priority', 'is_null', null)), {
    op: 'cmp',
    field: 'task.priority',
    cmp: 'is_null',
  });
  assert.deepEqual(toJson(cmp('task.priority', 'not_null', null)), {
    op: 'cmp',
    field: 'task.priority',
    cmp: 'not_null',
  });
});

test('附加：in 的右值必须是数组（标量会被体检拦下）', () => {
  const report = validateTree(cmp('form.tag', 'in', 'urgent'));
  assert.equal(report.ok, false);
  assert.ok(report.issues.some((issue) => /的右值必须是数组/.test(issue.message)));
  // 数组则通过
  assert.equal(validateTree(cmp('form.tag', 'in', ['urgent'])).ok, true);
});

test('附加：单组 args 到 16 项封顶（后端 MAX_CONDITION_ARGS）', () => {
  const tree = group('or', ...Array.from({ length: MAX_ARGS }, (_, i) => cmp('form.f', 'eq', i)));
  assert.equal(addBlock(tree, []).cmp, `同一分组最多 ${MAX_ARGS} 个条件`);
  assert.equal(addChild(tree, [], 'cmp'), null);
  assert.equal(validateTree(tree).ok, true, '16 项本身合法');
  // 15 项时还能加第 16 项
  const near = group('or', ...Array.from({ length: MAX_ARGS - 1 }, (_, i) => cmp('form.f', 'eq', i)));
  assert.equal(addBlock(near, []).cmp, null);
});

test('附加：addChild 造出来的分组自带一个空 cmp（点开就能填，且节点数 +2）', () => {
  const tree = group('and', cmp('form.amount', 'gte', 1));
  const before = countNodes(tree);
  const path = addChild(tree, [], 'group');
  assert.deepEqual(path, [1]);
  const added = nodeAtPath(tree, path ?? []);
  assert.ok(added && added.op === 'and');
  assert.equal((added as GroupNode).args.length, 1);
  assert.equal(countNodes(tree), before + 2);
  // 空分组里的空字段会被体检标红，但不会崩
  assert.equal(validateTree(tree).ok, false);
  assert.deepEqual(createGroupNode('not').args.length, 1);
  assert.deepEqual(createCmpNode(), { op: 'cmp', field: '', cmp: 'eq', value: '' });
});

test('附加：非对象/未知算子反解不抛异常（老数据防御）', () => {
  const cases: unknown[] = [
    [],
    'and',
    42,
    { op: 'xor', args: [] },
    { op: 'and' },
    { op: 'and', args: ['x', null] },
    { op: 'cmp', field: 'form.a', cmp: 'like', value: 1 },
    { op: 'cmp', field: 'form.a', cmp: 'eq' },
  ];
  for (const sample of cases) {
    const parsed = fromJson(sample);
    assert.ok(parsed.issues.length > 0, `脏数据应记问题：${JSON.stringify(sample)}`);
    // 能生成 JSON（或 null），不抛
    assert.doesNotThrow(() => toJson(parsed.node));
  }
  // 未知算子 → 树退化到 null，并由上层保留原文（不丢用户数据）
  assert.equal(fromJson({ op: 'xor', args: [] }).node, null);
});

test('附加：semanticKey 忽略键顺序（避免 watch 回灌冲掉正在编辑的输入）', () => {
  const a = { op: 'cmp', field: 'form.a', cmp: 'eq', value: 1 };
  const b = { value: 1, cmp: 'eq', field: 'form.a', op: 'cmp' };
  assert.notEqual(JSON.stringify(a), JSON.stringify(b));
  assert.equal(semanticKey(a), semanticKey(b));
  // 生成出来的 JSON 与后端原样（键顺序不同）也算「同一个值」→ 不重建树
  assert.equal(semanticKey(toJson(fromJson(b).node)), semanticKey(b));
});
